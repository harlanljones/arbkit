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

/// The runner's authoritative risk posture, exactly as its `RiskGate` holds
/// it. A `None` cap means this runner enforces no such limit — the dashboard
/// must render that honestly, never substitute a client-side default. Only a
/// runner that genuinely enforces the full envelope (per-leg cap, daily loss
/// budget, open-trade cap, edge floor) reports those fields as `Some`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RiskStateFrame {
    /// `paper` or `live`; made explicit before any order can flow.
    pub execution_mode: &'static str,
    /// Hard stop mirroring `RiskConfig::default().kill_switch`.
    pub kill_switch: bool,
    pub max_stake_per_leg_cents: Option<i64>,
    pub max_daily_loss_cents: Option<i64>,
    /// Realized loss consumed from the daily budget, positive number.
    pub daily_loss_used_cents: Option<i64>,
    pub max_open_trades: Option<u32>,
    pub open_trades: Option<u32>,
    pub min_edge_bps: Option<u32>,
}

impl RiskStateFrame {
    /// This paper runner's honest envelope: mode and kill switch are real,
    /// every cap is absent because nothing here enforces one.
    pub fn paper(kill_switch: bool) -> Self {
        Self {
            execution_mode: "paper",
            kill_switch,
            max_stake_per_leg_cents: None,
            max_daily_loss_cents: None,
            daily_loss_used_cents: None,
            max_open_trades: None,
            open_trades: None,
            min_edge_bps: None,
        }
    }
}

/// How far a reconciled fill has progressed into the authoritative ledger.
/// Part of the frozen wire contract even though the paper runner — whose
/// settlement is instantaneous — never emits one; that is contract
/// completeness, not dead design.
#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum SettlementStatus {
    Open,
    Settled,
    Unwound,
}

/// One fill event keyed by the execution layer's idempotency key. Realized
/// cents ride along only once settlement has actually reported them.
#[allow(dead_code)]
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FillRecord {
    pub client_order_id: String,
    pub venue_order_id: Option<String>,
    /// Links the fill to its streamed [`TradeRecord`] when one exists.
    pub trade_seq: Option<u64>,
    pub filled_stake_cents: i64,
    pub realized_profit_cents: Option<i64>,
    pub settlement_status: SettlementStatus,
    pub reconciled_at_epoch_ms: u128,
}

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
        /// `paper` or `live`, stated up front. Absent on pre-extension
        /// runners, which the dashboard must treat as mode unknown.
        #[serde(skip_serializing_if = "Option::is_none")]
        execution_mode: Option<&'static str>,
    },
    /// The runner's own risk posture, sent at session open and whenever a
    /// command (or the runner itself) moves it. The dashboard's kill-switch
    /// pill and risk envelope are built from exactly this frame.
    #[serde(rename = "risk")]
    Risk { state: RiskStateFrame },
    /// Fill events as they reconcile into the authoritative ledger, keyed by
    /// client order id. Paper settlement is instantaneous, so paper runners
    /// stream none; live reconciliation streams one per venue fill. Kept in
    /// the paper runner's enum so the wire contract has exactly one shape.
    #[serde(rename = "fills")]
    #[allow(dead_code)]
    Fills { items: Vec<FillRecord> },
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
        /// Micro-live execution counters. Zero on the paper runner — its
        /// settlement is instantaneous, so nothing is ever in flight, nothing
        /// can fail to unwind, and acks are trivially matched. Present so the
        /// wire contract has exactly one shape.
        #[allow(dead_code)]
        unwind_failures: u64,
        #[allow(dead_code)]
        ack_matched: u64,
        #[allow(dead_code)]
        in_flight_remaining: usize,
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
            LiveFrame::Risk { .. } => "risk",
            LiveFrame::Fills { .. } => "fills",
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
            LiveFrame::Fills { items } => items.len(),
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
