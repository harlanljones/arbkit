//! Live runner stream tests.
//!
//! The runner's example modules are pulled in by path so these behaviors run
//! under `cargo test --workspace` like any other target: batching triggers,
//! retry/drop accounting, heartbeat liveness, the frozen wire shape, and one
//! full session over real HTTP against a throwaway mock ingest. The mock is
//! a raw `TcpListener`, not a framework — the fewer moving parts between the
//! writer thread and the truth, the more the test proves.

#[path = "../examples/live_runner/frames.rs"]
mod frames;

#[path = "../examples/live_runner/stream.rs"]
mod stream;

#[path = "../examples/trades_ledger/mod.rs"]
mod trades_ledger;

use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use frames::{LiveFrame, RiskStateFrame, LIVE_SCHEMA_VERSION};
use stream::{StreamConfig, StreamHandle};
use trades_ledger::TradeRecord;

/// A minimal but valid trade record. Every field mirrors what
/// `build_trade_record` emits; the values themselves only matter where a
/// test asserts on them (seq, labels).
fn sample_record(seq: u64) -> TradeRecord {
    TradeRecord {
        seq,
        detection_timestamp_ns: 1_000 + seq,
        latency_ns: 5_000,
        market_label: "Boston Celtics @ Los Angeles Lakers · moneyline".into(),
        edge_bps: 200,
        overround_ppm: 980_000,
        requested_stake_cents: 99_999,
        expected_profit_cents: 2_040,
        worst_case_profit_cents: 2_001,
        realized_profit_cents: 1_800,
        slippage_cents: 240,
        fees_paid_cents: 175,
        fill_ratio_bps: 9_500,
        classification: "proportional".into(),
        chased: false,
        legs: Vec::new(),
    }
}

fn patient_config(url: String) -> StreamConfig {
    StreamConfig {
        url,
        token: "test-token".into(),
        // Long intervals by default so a test that waits for a specific
        // trigger knows exactly which policy fired.
        flush_interval: Duration::from_secs(60),
        heartbeat_interval: Duration::from_secs(60),
        ..Default::default()
    }
}

fn wait_until(predicate: impl Fn() -> bool, what: &str) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !predicate() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn frames_serialize_to_the_frozen_wire_shape() {
    let start = LiveFrame::SessionStart {
        schema_version: LIVE_SCHEMA_VERSION,
        run_id: "run-1".into(),
        started_at_epoch_ms: 5,
        initial_bankroll_cents: Some(10_000),
        ticks_per_window: 200,
        window_ms: 1_000,
        execution_mode: Some("paper"),
    };
    let line = start.to_ndjson_line().expect("session-start serializes");
    assert!(line.starts_with("{\"t\":\"session-start\""), "{line}");
    assert!(line.contains("\"runId\":\"run-1\""), "{line}");
    assert!(
        line.contains("\"initialBankrollCents\":10000"),
        "camelCase money field missing: {line}"
    );
    assert!(line.contains("\"executionMode\":\"paper\""), "{line}");
    assert!(line.ends_with("}\n"), "one NDJSON line per frame");

    let end = LiveFrame::SessionEnd.to_ndjson_line().unwrap();
    assert_eq!(end.trim(), "{\"t\":\"session-end\"}");

    let record = sample_record(3);
    let positions = LiveFrame::Positions {
        items: vec![record.clone()],
    };
    let line = positions.to_ndjson_line().unwrap();
    let parsed: serde_json::Value = serde_json::from_str(line.trim()).unwrap();
    assert_eq!(parsed["items"][0]["seq"], 3);
    assert_eq!(
        parsed["items"][0]["marketLabel"],
        serde_json::Value::String(record.market_label.clone())
    );
    assert_eq!(
        parsed["items"][0]["worstCaseProfitCents"],
        record.worst_case_profit_cents
    );

    // The risk posture frame carries the runner's own envelope verbatim:
    // camelCase fields, kill switch as a boolean, absent caps as nulls.
    let risk = LiveFrame::Risk {
        state: RiskStateFrame::paper(false),
    };
    let line = risk.to_ndjson_line().unwrap();
    let parsed: serde_json::Value = serde_json::from_str(line.trim()).unwrap();
    assert_eq!(parsed["state"]["executionMode"], "paper");
    assert_eq!(parsed["state"]["killSwitch"], false);
    assert_eq!(
        parsed["state"]["maxStakePerLegCents"],
        serde_json::Value::Null
    );
    assert_eq!(risk.kind(), "risk");
    assert_eq!(risk.record_count(), 0);

    // Accessors agree with the wire tags they mirror.
    assert_eq!(positions.kind(), "positions");
    assert_eq!(positions.record_count(), 1);
    assert_eq!(start.kind(), "session-start");
    assert_eq!(start.record_count(), 0);
}

#[test]
fn size_trigger_flushes_before_the_interval_elapses() {
    let captured: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&captured);
    let transport = Box::new(move |_url: &str, _token: &str, body: &str| {
        sink.lock().unwrap().push(body.to_string());
        Ok(())
    });

    let handle = StreamHandle::spawn_with_transport(
        StreamConfig {
            flush_records: 2,
            ..patient_config("mock://ingest".into())
        },
        Some(transport),
    );

    // Two position frames carry three records; the threshold of two records
    // trips mid-second-frame-batch — the whole frame ships, not a slice.
    handle.send(LiveFrame::Positions {
        items: vec![sample_record(0)],
    });
    handle.send(LiveFrame::Positions {
        items: vec![sample_record(1), sample_record(2)],
    });

    wait_until(
        || !captured.lock().unwrap().is_empty(),
        "size-triggered flush",
    );
    let bodies = captured.lock().unwrap();
    assert_eq!(bodies.len(), 1, "one batch, not one per frame");
    let lines: Vec<&str> = bodies[0].lines().collect();
    assert_eq!(lines.len(), 2, "both buffered frames ship together");
    assert!(lines[0].contains("\"t\":\"positions\""));

    let summary = handle.stop(false);
    assert_eq!(summary.batches_sent, 1);
    assert_eq!(summary.records_sent, 3);
    assert_eq!(summary.frames_dropped, 0);
}

#[test]
fn time_trigger_flushes_a_lone_frame() {
    let captured: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&captured);
    let transport = Box::new(move |_url: &str, _token: &str, body: &str| {
        sink.lock().unwrap().push(body.to_string());
        Ok(())
    });

    let handle = StreamHandle::spawn_with_transport(
        StreamConfig {
            flush_interval: Duration::from_millis(120),
            ..patient_config("mock://ingest".into())
        },
        Some(transport),
    );
    handle.send(LiveFrame::Stats {
        seq_cursor: 7,
        windows_completed: 1,
        locked_cents: Some(1_000),
        available_cents: Some(9_000),
        attempted: 1,
        capital_short: 0,
        unwind_failures: 0,
        ack_matched: 0,
        in_flight_remaining: 0,
    });

    wait_until(
        || !captured.lock().unwrap().is_empty(),
        "time-triggered flush",
    );
    let body = captured.lock().unwrap()[0].clone();
    assert!(body.contains("\"seqCursor\":7"), "{body}");

    let summary = handle.stop(false);
    assert_eq!(summary.batches_sent, 1);
    assert_eq!(summary.records_sent, 0, "stats carry no trade records");
}

#[test]
fn undeliverable_batches_are_retried_then_counted_as_dropped() {
    let attempts = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&attempts);
    let transport = Box::new(move |_url: &str, _token: &str, _body: &str| {
        counter.fetch_add(1, Ordering::SeqCst);
        Err(String::from("mock ingest down"))
    });

    let handle = StreamHandle::spawn_with_transport(
        StreamConfig {
            max_retries: 2,
            ..patient_config("mock://ingest".into())
        },
        Some(transport),
    );
    handle.send(LiveFrame::Positions {
        items: vec![sample_record(0)],
    });
    let summary = handle.stop(true); // graceful stop forces the flush attempt

    // One initial attempt plus `max_retries` retries.
    assert_eq!(attempts.load(Ordering::SeqCst), 3, "bounded retries");
    assert_eq!(summary.batches_dropped, 1);
    assert_eq!(summary.records_dropped, 1);
    assert_eq!(summary.batches_sent, 0);
}

#[test]
fn heartbeats_flow_when_the_session_goes_quiet() {
    let captured: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&captured);
    let transport = Box::new(move |_url: &str, _token: &str, body: &str| {
        sink.lock().unwrap().push(body.to_string());
        Ok(())
    });

    let handle = StreamHandle::spawn_with_transport(
        StreamConfig {
            heartbeat_interval: Duration::from_millis(80),
            ..patient_config("mock://ingest".into())
        },
        Some(transport),
    );
    handle.cursor().store(41, Ordering::Relaxed);
    handle.send(LiveFrame::SessionStart {
        schema_version: LIVE_SCHEMA_VERSION,
        run_id: "run-quiet".into(),
        started_at_epoch_ms: 1,
        initial_bankroll_cents: None,
        ticks_per_window: 200,
        window_ms: 1_000,
        execution_mode: Some("paper"),
    });

    wait_until(
        || {
            captured
                .lock()
                .unwrap()
                .iter()
                .any(|body| body.contains("\"t\":\"heartbeat\""))
        },
        "heartbeat during silence",
    );
    let bodies = captured.lock().unwrap();
    let beat = bodies
        .iter()
        .find(|body| body.contains("\"t\":\"heartbeat\""))
        .unwrap();
    assert!(beat.contains("\"seqCursor\":41"), "{beat}");
}

/// Reads exactly one HTTP/1.1 request off `stream`, returning
/// `(authorization header, body)`. Content-Length framed bodies only —
/// that is all the runner ever sends.
fn read_http_request(stream: &mut std::net::TcpStream) -> Option<(Option<String>, String)> {
    let mut buffer = Vec::new();
    let mut chunk = [0u8; 1024];
    while !buffer.windows(4).any(|window| window == b"\r\n\r\n") {
        let read = stream.read(&mut chunk).ok()?;
        if read == 0 {
            return None;
        }
        buffer.extend_from_slice(&chunk[..read]);
    }
    let split = buffer
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .expect("loop exits only after headers complete");
    let head = String::from_utf8_lossy(&buffer[..split]).to_string();
    let content_length = head
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case("content-length")
                .then(|| value.trim().parse::<usize>().ok())?
        })
        .unwrap_or(0);

    let mut body = buffer[split + 4..].to_vec();
    while body.len() < content_length {
        let read = stream.read(&mut chunk).ok()?;
        if read == 0 {
            break;
        }
        body.extend_from_slice(&chunk[..read]);
    }
    body.truncate(content_length);

    let authorization = head.lines().find_map(|line| {
        let (name, value) = line.split_once(':')?;
        name.eq_ignore_ascii_case("authorization")
            .then(|| value.trim().to_string())
    });
    Some((
        authorization,
        String::from_utf8(body).expect("NDJSON is utf-8"),
    ))
}

#[test]
fn streams_a_full_session_over_real_http() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("ephemeral port binds");
    let addr = listener.local_addr().unwrap();

    let server = thread::spawn(move || {
        listener
            .set_nonblocking(true)
            .expect("listener accepts non-blocking");
        let mut received: Vec<(Option<String>, String)> = Vec::new();
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if Instant::now() > deadline {
                break;
            }
            match listener.accept() {
                Ok((mut socket, _)) => {
                    socket.set_nonblocking(false).expect("request reads block");
                    if let Some(request) = read_http_request(&mut socket) {
                        let saw_end = request.1.contains("\"t\":\"session-end\"");
                        let _ = socket.write_all(
                            b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                        );
                        received.push(request);
                        if saw_end {
                            break;
                        }
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_millis(20));
                }
                Err(_) => break,
            }
        }
        received
    });

    let handle = StreamHandle::spawn(StreamConfig {
        url: format!("http://{addr}/api/live/ingest"),
        token: "secret-token".into(),
        flush_interval: Duration::from_millis(120),
        heartbeat_interval: Duration::from_millis(600),
        ..Default::default()
    });

    handle.send(LiveFrame::SessionStart {
        schema_version: LIVE_SCHEMA_VERSION,
        run_id: "run-http".into(),
        started_at_epoch_ms: 9,
        initial_bankroll_cents: Some(300_000),
        ticks_per_window: 200,
        window_ms: 1_000,
        execution_mode: Some("paper"),
    });
    handle.send(LiveFrame::Positions {
        items: vec![sample_record(8), sample_record(9)],
    });
    handle.cursor().store(10, Ordering::Relaxed);
    handle.send(LiveFrame::Stats {
        seq_cursor: 10,
        windows_completed: 1,
        locked_cents: Some(99_999),
        available_cents: Some(200_001),
        attempted: 2,
        capital_short: 0,
        unwind_failures: 0,
        ack_matched: 0,
        in_flight_remaining: 0,
    });
    handle.send(LiveFrame::SessionEnd);

    let summary = handle.stop(true);
    assert_eq!(
        summary.frames_dropped, 0,
        "every frame must land against a healthy ingest"
    );

    let received = server.join().expect("mock ingest joins");
    assert!(!received.is_empty(), "the mock must have seen requests");
    assert!(
        received
            .iter()
            .any(|(auth, _)| auth.as_deref() == Some("Bearer secret-token")),
        "credentials ride on every request"
    );

    let wire = received
        .iter()
        .map(|(_, body)| body.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    let start_at = wire.find("\"t\":\"session-start\"").expect("session opens");
    let positions_at = wire.find("\"t\":\"positions\"").expect("trades arrive");
    let stats_at = wire.find("\"t\":\"stats\"").expect("stats arrive");
    let end_at = wire.rfind("\"t\":\"session-end\"").expect("session closes");
    assert!(start_at < positions_at && positions_at < stats_at && stats_at < end_at);

    for line in wire.lines().filter(|line| !line.is_empty()) {
        let parsed: serde_json::Value =
            serde_json::from_str(line).expect("every delivered line is JSON");
        assert!(parsed.get("t").is_some(), "every line carries its tag");
    }
}
