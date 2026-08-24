//! Wire envelopes for the live position stream.
//!
//! One NDJSON frame per line, POSTed by the writer thread in
//! [`crate::stream`] and mirrored on the dashboard by zod schemas built from
//! exactly these shapes. The per-trade payload is the frozen
//! `TradeRecord` contract (`trades_ledger`), reused verbatim so a streamed
//! position and a ledger line are the same bytes — one schema to freeze,
//! one place for pessimistic numbers to survive serialization untouched.
//!
//! Money stays integer cents and rates stay integer bps/ppm end to end; no
//! float ever touches a value that decides or reports anything.

use serde::Serialize;

use crate::trades_ledger::TradeRecord;

/// Schema version of the live frame protocol. Bump on any shape change.
pub const LIVE_SCHEMA_VERSION: u32 = 1;

/// One message on the runner → ingest wire. Internally tagged with `"t"` so
/// a reader can dispatch on the first bytes it needs regardless of framing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "t", rename_all_fields = "camelCase")]
pub enum LiveFrame {
    /// Opens a session. The ingest endpoint treats everything before this as
    /// unsolicited and everything long after the last heartbeat as dead.
    #[serde(rename = "session-start")]
    SessionStart {
        schema_version: u32,
        run_id: String,
        started_at_epoch_ms: u128,
        /// Starting capital across all venues; `None` in static-budget mode.
        initial_bankroll_cents: Option<i64>,
        ticks_per_window: usize,
        window_ms: u64,
    },
    /// A batch of completed paper trades, detection order preserved. Each
    /// item already carries its realized outcome: the sim settles at fill
    /// time, so "locked" here means capital committed until settlement,
    /// which is exactly what the record's fields report.
    #[serde(rename = "positions")]
    Positions { items: Vec<TradeRecord> },
    /// Runner-side aggregates that no sum of records can recover: bankroll
    /// utilization (capital locked vs available) and the disposition funnel
    /// entries (attempted / capital-short) for trades that never became
    /// records because capital was short before an order was sent.
    #[serde(rename = "stats")]
    Stats {
        seq_cursor: u64,
        windows_completed: usize,
        locked_cents: Option<i64>,
        available_cents: Option<i64>,
        attempted: u64,
        capital_short: u64,
    },
    /// Liveness proof between batches. The dashboard marks a session stale
    /// when these stop arriving; there is no graceful-shutdown channel.
    #[serde(rename = "heartbeat")]
    Heartbeat { seq_cursor: u64 },
    /// Best-effort final marker for finite runs. A killed runner simply
    /// never sends it — staleness handles that case.
    #[serde(rename = "session-end")]
    SessionEnd,
}

impl LiveFrame {
    /// The wire tag for this frame, matching the serde rename above.
    pub fn kind(&self) -> &'static str {
        match self {
            LiveFrame::SessionStart { .. } => "session-start",
            LiveFrame::Positions { .. } => "positions",
            LiveFrame::Stats { .. } => "stats",
            LiveFrame::Heartbeat { .. } => "heartbeat",
            LiveFrame::SessionEnd => "session-end",
        }
    }

    /// How many trade records the frame carries. Size-based flush policy is
    /// expressed in records, not frames, so one huge batch cannot hide
    /// behind a small frame count.
    pub fn record_count(&self) -> usize {
        match self {
            LiveFrame::Positions { items } => items.len(),
            _ => 0,
        }
    }

    /// Serializes to one NDJSON line (trailing newline included).
    pub fn to_ndjson_line(&self) -> Result<String, String> {
        serde_json::to_string(self)
            .map(|mut line| {
                line.push('\n');
                line
            })
            .map_err(|error| format!("could not serialize {} frame: {error}", self.kind()))
    }
}
