//! The runner-side streaming writer: batches frames, POSTs NDJSON, stays out
//! of everything else's way.
//!
//! Lives beside the pipeline examples because it pairs collected trades with
//! an outbound socket — the same post-consumption tier as `trades_ledger`,
//! never the hot path. The engine loop hands records over an mpsc channel to
//! one dedicated writer thread, which coalesces them into micro-batches
//! (size- or time-triggered, whichever first) and delivers each batch with
//! bounded retries. A batch it cannot deliver is counted and dropped, never
//! queued without bound: this is a live view, and a stale flood is worse
//! than an honest gap.
//!
//! Liveness is carried by heartbeats emitted here when the frame channel
//! goes quiet — there is deliberately no signal handler upstream, so a killed
//! runner simply stops beating and the dashboard marks the session stale.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crate::frames::LiveFrame;

/// How often the writer wakes to check flush/heartbeat deadlines. Bounds the
/// added latency on top of [`StreamConfig::flush_interval`].
const POLL_GRANULARITY: Duration = Duration::from_millis(50);

/// Retry backoff schedule base: 250ms, 500ms, 1s, capped at 2s thereafter.
const RETRY_BACKOFF_MS: u64 = 250;

/// Everything the writer needs to know.
#[derive(Debug, Clone)]
pub struct StreamConfig {
    /// Full ingest endpoint URL (`POST`, NDJSON body).
    pub url: String,
    /// Bearer token; empty string omits the header entirely (local mocks).
    pub token: String,
    /// Flush once this many trade records are buffered, regardless of time.
    pub flush_records: usize,
    /// Flush whatever is buffered after this long since the first frame.
    pub flush_interval: Duration,
    /// Emit a heartbeat after this much wire silence mid-session.
    pub heartbeat_interval: Duration,
    /// Per-request overall timeout.
    pub request_timeout: Duration,
    /// Retries per batch after its first delivery attempt fails. A value of
    /// 2 means up to three requests for one batch.
    pub max_retries: u32,
}

impl Default for StreamConfig {
    fn default() -> Self {
        Self {
            url: String::from("http://127.0.0.1:8787/api/live/ingest"),
            token: String::new(),
            flush_records: 64,
            flush_interval: Duration::from_millis(500),
            heartbeat_interval: Duration::from_millis(5_000),
            request_timeout: Duration::from_secs(5),
            max_retries: 4,
        }
    }
}

/// What the writer accomplished, reported when the stream shuts down. Dropped
/// counts are the honest-gap numbers: they belong in the operator console,
/// not silently in a log file.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct StreamSummary {
    pub batches_sent: u64,
    pub frames_sent: u64,
    pub records_sent: u64,
    pub heartbeats_sent: u64,
    pub batches_dropped: u64,
    pub frames_dropped: u64,
    pub records_dropped: u64,
}

/// Outbound transport, injectable so every batching/retry behavior is
/// testable against a capturing closure instead of a socket. Arguments are
/// `(url, token, ndjson_body)`.
type Transport = dyn Fn(&str, &str, &str) -> Result<(), String> + Send;

enum StreamMsg {
    Frame(LiveFrame),
    Finish,
}

/// Handle to the running writer thread. Cheap to hold; `send` never blocks
/// the caller meaningfully (the channel is unbounded by design — the writer
/// drains it far faster than paper trades are produced).
pub struct StreamHandle {
    tx: Option<Sender<StreamMsg>>,
    cursor: Arc<AtomicU64>,
    join: Option<JoinHandle<StreamSummary>>,
}

impl StreamHandle {
    /// Spawns the writer with the real HTTP transport.
    pub fn spawn(config: StreamConfig) -> Self {
        Self::spawn_with_transport(config, None)
    }

    /// Spawns the writer with an injected transport (tests only pass this).
    pub fn spawn_with_transport(config: StreamConfig, transport: Option<Box<Transport>>) -> Self {
        let transport = transport.unwrap_or_else(|| default_transport(&config));
        let cursor = Arc::new(AtomicU64::new(0));
        let cursor_writer = Arc::clone(&cursor);
        let (tx, rx) = mpsc::channel::<StreamMsg>();
        let join = std::thread::Builder::new()
            .name("live-stream-writer".into())
            .spawn(move || writer_loop(&config, &*transport, rx, cursor_writer))
            .expect("writer thread spawns");
        Self {
            tx: Some(tx),
            cursor,
            join: Some(join),
        }
    }

    /// Shared sequence cursor: the producer updates it as records complete so
    /// heartbeats can report progress during silence.
    pub fn cursor(&self) -> &Arc<AtomicU64> {
        &self.cursor
    }

    /// Queues a frame for delivery. Errors are ignored by contract: a closed
    /// channel means the writer already stopped, and the summary will say so.
    pub fn send(&self, frame: LiveFrame) {
        if let Some(tx) = &self.tx {
            let _ = tx.send(StreamMsg::Frame(frame));
        }
    }

    /// Shuts the writer down. Graceful mode delivers buffered frames first;
    /// either way the returned summary is the run's delivery record.
    pub fn stop(mut self, graceful: bool) -> StreamSummary {
        if graceful {
            if let Some(tx) = &self.tx {
                let _ = tx.send(StreamMsg::Finish);
            }
        }
        // Dropping the sender also ends an ungraceful stop via Disconnected.
        self.tx = None;
        self.join
            .take()
            .and_then(|join| join.join().ok())
            .unwrap_or_default()
    }
}

impl Drop for StreamHandle {
    fn drop(&mut self) {
        // Best-effort only: never block a dropping thread on the writer.
        self.tx = None;
    }
}

fn writer_loop(
    config: &StreamConfig,
    transport: &Transport,
    rx: mpsc::Receiver<StreamMsg>,
    cursor: Arc<AtomicU64>,
) -> StreamSummary {
    let mut summary = StreamSummary::default();
    let mut buffer: Vec<LiveFrame> = Vec::new();
    let mut buffered_records = 0usize;
    let mut oldest_buffered: Option<Instant> = None;
    let mut last_wire_activity = Instant::now();
    let mut session_active = false;

    loop {
        match rx.recv_timeout(POLL_GRANULARITY) {
            Ok(StreamMsg::Frame(frame)) => {
                let starting = matches!(frame, LiveFrame::SessionStart { .. });
                if starting {
                    session_active = true;
                }
                let ending = matches!(frame, LiveFrame::SessionEnd);
                buffered_records += frame.record_count();
                buffer.push(frame);
                oldest_buffered.get_or_insert_with(Instant::now);
                if starting || ending {
                    // Session boundaries ship immediately: the ingest must
                    // learn a session opened (and that heartbeats may follow)
                    // without waiting out a batching window.
                    flush_buffer(
                        config,
                        transport,
                        &mut buffer,
                        &mut buffered_records,
                        &mut last_wire_activity,
                        &mut summary,
                    );
                }
            }
            Ok(StreamMsg::Finish) => {
                // Explicit graceful stop from `stop(true)`: flush, then exit.
                flush_buffer(
                    config,
                    transport,
                    &mut buffer,
                    &mut buffered_records,
                    &mut last_wire_activity,
                    &mut summary,
                );
                break;
            }
            Err(RecvTimeoutError::Disconnected) => {
                // Sender gone without a graceful stop: count the gap honestly.
                summary.frames_dropped += buffer.len() as u64;
                summary.records_dropped += buffered_records as u64;
                break;
            }
            Err(RecvTimeoutError::Timeout) => {}
        }

        let size_due = buffered_records >= config.flush_records;
        let time_due =
            oldest_buffered.is_some_and(|oldest| oldest.elapsed() >= config.flush_interval);
        if size_due || time_due {
            flush_buffer(
                config,
                transport,
                &mut buffer,
                &mut buffered_records,
                &mut last_wire_activity,
                &mut summary,
            );
        } else if session_active
            && buffer.is_empty()
            && last_wire_activity.elapsed() >= config.heartbeat_interval
        {
            let beat = LiveFrame::Heartbeat {
                seq_cursor: cursor.load(Ordering::Relaxed),
            };
            if deliver_batch(config, transport, &[beat]).is_ok() {
                summary.heartbeats_sent += 1;
                last_wire_activity = Instant::now();
            }
        }
    }

    summary
}

/// Ships everything buffered as one NDJSON batch, clearing state either way.
fn flush_buffer(
    config: &StreamConfig,
    transport: &Transport,
    buffer: &mut Vec<LiveFrame>,
    buffered_records: &mut usize,
    last_wire_activity: &mut Instant,
    summary: &mut StreamSummary,
) {
    if buffer.is_empty() {
        return;
    }
    let frames = std::mem::take(buffer);
    *buffered_records = 0;
    if deliver_batch(config, transport, &frames).is_ok() {
        summary.batches_sent += 1;
        summary.frames_sent += frames.len() as u64;
        summary.records_sent += frames.iter().map(LiveFrame::record_count).sum::<usize>() as u64;
        *last_wire_activity = Instant::now();
    } else {
        summary.batches_dropped += 1;
        summary.frames_dropped += frames.len() as u64;
        summary.records_dropped += frames.iter().map(LiveFrame::record_count).sum::<usize>() as u64;
    }
}

/// Delivers one NDJSON batch with bounded retries. Returns `Err(())` only
/// after every attempt failed; the batch contents are then the caller's to
/// count as dropped.
fn deliver_batch(
    config: &StreamConfig,
    transport: &Transport,
    frames: &[LiveFrame],
) -> Result<(), ()> {
    let mut body = String::new();
    for frame in frames {
        match frame.to_ndjson_line() {
            Ok(line) => body.push_str(&line),
            Err(error) => eprintln!("[live-stream] {error}; frame skipped"),
        }
    }
    let mut attempt = 0u32;
    loop {
        attempt += 1;
        match transport(&config.url, &config.token, &body) {
            Ok(()) => return Ok(()),
            Err(error) => {
                if attempt > config.max_retries {
                    eprintln!(
                        "[live-stream] dropped {} frame(s) after {attempt} attempts: {error}",
                        frames.len()
                    );
                    return Err(());
                }
                eprintln!(
                    "[live-stream] attempt {attempt}/{} failed: {error}",
                    config.max_retries
                );
                let shift = attempt.saturating_sub(1).min(3);
                std::thread::sleep(Duration::from_millis(RETRY_BACKOFF_MS << shift));
            }
        }
    }
}

/// Builds the production transport: a blocking HTTPS-capable POST with a
/// fixed agent. Plain-text URLs work identically for local development.
fn default_transport(config: &StreamConfig) -> Box<Transport> {
    let agent = ureq::AgentBuilder::new()
        .timeout(config.request_timeout)
        .user_agent("arbkit-live-runner")
        .build();
    Box::new(move |url, token, body| {
        let mut request = agent.post(url).set("Content-Type", "application/x-ndjson");
        if !token.is_empty() {
            request = request.set("Authorization", &format!("Bearer {token}"));
        }
        match request.send_string(body) {
            Ok(_) => Ok(()),
            Err(ureq::Error::Status(code, _)) => Err(format!("ingest returned HTTP {code}")),
            Err(error) => Err(format!("ingest transport error: {error}")),
        }
    })
}
