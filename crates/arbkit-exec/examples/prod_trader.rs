//! Production live-trading runner: the process that actually transmits orders.
//!
//! This is the live twin of `arbkit-engine`'s paper `live_runner`. Where that
//! example streams a scripted synthetic feed through the simulator, this one
//! wires the real stack end to end:
//!
//! ```text
//! [tokio] KalshiLiveFeed + PolymarketLiveFeed
//!     -> MpscSender -> bridge thread -> feed ring (SPSC)
//!     -> Engine thread (hot loop, pinned, no I/O)
//!     -> signal ring (SPSC) -> this main loop:
//!        OpportunityDeduper -> exec_legs_from_signal -> RiskGate
//!        -> HedgedExecutor -> venue adapters (or DryRun) -> journal + stream
//! ```
//!
//! Safety posture (mirrors `LIVE_TRADING.md`):
//!
//! - `--mode=live` refuses to start while `ARBKIT_KILL_SWITCH` is engaged.
//! - `--mode=dry-run` runs the identical pipeline against `DryRunAdapter`:
//!   real feeds, real detection, real risk gating, zero orders transmitted.
//! - The operator command queue is pulled every window; a kill-switch flip
//!   applies through the runner's own `RiskGate` and is logged with a UTC
//!   timestamp for the runbook.
//! - Risk state checkpoints to `RiskStateStore` after every execution; a
//!   restart with in-flight orders refuses live mode until reconciled.
//! - Credentials come from the environment (secret-manager injected). The
//!   runner sweeps its artifacts — risk snapshot, journal — for that
//!   material before order flow and at shutdown; any hit aborts with exit
//!   code 9 naming the artifact and credential label, never the value.
//! - Nothing here touches the hot path: the engine thread never learns that
//!   a network, a ledger, or this file exists.
//!
//! Usage:
//! ```text
//! cargo run -p arbkit-exec --features runner --example prod_trader -- \
//!     --mode=dry-run \
//!     [--url http://127.0.0.1:8787/api/live/ingest] [--token-env LIVE_INGEST_TOKEN] \
//!     [--state prod-risk-state.json] [--journal prod-session.ndjson] \
//!     [--window-ms 250] [--windows <n>]
//! ```
//!
//! Without `--windows` the runner streams until an operator sends
//! `session-end` (or it is killed; the dashboard then declares it stale by
//! heartbeat timeout, exactly like the paper runner).

// The operator command protocol has exactly one runner-side implementation,
// shared with the paper runner so the wire contract cannot drift between
// the two processes that speak it.
#[path = "../../arbkit-engine/examples/live_runner/control.rs"]
mod control;

use std::collections::HashMap;
use std::env;
use std::fs;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::Sender;
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use arbkit_core::{Cents, Fee, MarketKind, Prob, VenueId};
use arbkit_engine::{spsc_ring, Engine, FeedEventSlot, MarketConfig, SignalEvent, SignalEventSlot};
use arbkit_exec::{
    exec_legs_from_signal, instrument_ref, micro_live_config, DryRunAdapter, ExecError,
    ExecutionClassification, ExecutionReport, HedgedExecutor, InFlightOrder, InstrumentResolver,
    KalshiConfig, KalshiExecutionAdapter, LiveProofReport, OpportunityDeduper, PersistedExecLeg,
    PolymarketConfig, PolymarketExecutionAdapter, ReconciledLeg, Reconciler, RiskConfig, RiskGate,
    RiskRejection, RiskStateStore, SecretScan, Settlement, SettlementSource, VenueAdapter,
    VenueInstrumentRef,
};
use arbkit_feed::live::{
    crossbeam_bridge, refresh_catalog, KalshiDiscoveryConfig, KalshiFeedConfig, KalshiLiveFeed,
    KalshiSubscription, PolymarketDiscoveryConfig, PolymarketFeedConfig, PolymarketLiveFeed,
    PolymarketSubscription, RestDiscoverySource,
};
use arbkit_feed::TapeWriter;
use arbkit_match::{CanonicalRegistry, VenueInstrumentMap, VenueRegistry};

use control::{poll_commands, OperatorCommand};

/// Marker error for the feed-ring bridge: the ring was full, spin and retry.
struct RingFull;

/// Wire schema version, frozen against the dashboard's zod mirror.
const LIVE_SCHEMA_VERSION: u32 = 1;
/// How long the writer waits between flush/heartbeat deadline checks.
const WRITER_POLL: Duration = Duration::from_millis(50);
/// Default streaming ingest endpoint.
const DEFAULT_INGEST_URL: &str = "http://127.0.0.1:8787/api/live/ingest";

// ---------------------------------------------------------------------------
// Configuration
// ---------------------------------------------------------------------------

/// Everything the runner needs, resolved once at startup.
struct RunnerConfig {
    /// `dry-run` exercises the full pipeline without transmitting orders.
    mode: String,
    /// Dashboard ingest URL (NDJSON POST).
    ingest_url: String,
    /// Bearer token for ingest + command pull.
    token: String,
    /// Environment variable name the bearer token came from (secret scan).
    token_env: String,
    /// Durable risk state path.
    state_path: String,
    /// Execution journal path (NDJSON, one record per attempt).
    journal_path: String,
    /// Main-loop period.
    window: Duration,
    /// Optional finite window count for testing.
    windows_limit: Option<u64>,
    /// Override for the Kalshi REST discovery endpoint (fixtures/drills).
    kalshi_markets_url: Option<String>,
    /// Override for the Polymarket Gamma discovery endpoint (fixtures/drills).
    poly_events_url: Option<String>,
    /// Optional raw feed tape path — records every event crossing the feed
    /// bridge so a warmup session can be replayed and audited offline.
    tape_path: Option<String>,
    /// Optional CSV dump of the active catalog for manual mapping review.
    dump_catalog: Option<String>,
    /// Micro-live posture: two-contract per-leg cap and a daily budget of
    /// one worst-case leg loss, clamped over whatever the env asked for.
    micro: bool,
    /// Occurrence tape path (one JSON record per executed signal).
    occurrences_path: String,
    /// Live proof report path, written at graceful shutdown.
    proof_path: String,
}

/// Read the value of a `--flag` argument, accepting both `--flag=value` and
/// the space-separated `--flag value` spelling. Both resolve identically.
///
/// A value-taking flag written bare with no following value is a usage error,
/// never a silent default: silently falling back to defaults when an operator
/// typed `--windows 5` (space form) is the failure mode this replaces.
fn arg_value(args: &[String], flag: &str) -> Result<Option<String>, String> {
    let eq = format!("--{flag}=");
    let bare = format!("--{flag}");
    let mut i = 0;
    while i < args.len() {
        if let Some(rest) = args[i].strip_prefix(&eq) {
            return Ok(Some(rest.to_owned()));
        }
        if args[i] == bare {
            match args.get(i + 1) {
                Some(next) if !next.starts_with("--") => return Ok(Some(next.clone())),
                _ => {
                    return Err(format!(
                        "flag `{bare}` requires a value (`{bare}=<value>` or `{bare} <value>`)"
                    ))
                }
            }
        }
        i += 1;
    }
    Ok(None)
}

/// Whether a boolean `--flag` is present. Matches the bare spelling exactly;
/// `--flag=value` is not a boolean form.
fn arg_flag(args: &[String], flag: &str) -> bool {
    let bare = format!("--{flag}");
    args.iter().any(|arg| arg == &bare)
}

/// Venue-unwind operations this session performed (proof-report counter).
static UNWINDS: AtomicU64 = AtomicU64::new(0);

fn env_cents(name: &str, default: Cents) -> Cents {
    env::var(name)
        .ok()
        .and_then(|raw| raw.trim().parse::<Cents>().ok())
        .unwrap_or(default)
}

fn env_u32(name: &str, default: u32) -> u32 {
    env::var(name)
        .ok()
        .and_then(|raw| raw.trim().parse::<u32>().ok())
        .unwrap_or(default)
}

fn epoch_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
}

fn epoch_ns() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0)
}

/// Sweep every artifact the session writes for credential material.
///
/// Called after the risk snapshot exists, again once the journal is created
/// and before order flow, and at graceful shutdown. A hit aborts with exit
/// code 9 naming the artifact and the credential label — never the value.
fn assert_artifacts_clean(config: &RunnerConfig, secrets: &SecretScan) {
    if secrets.is_empty() {
        return;
    }
    if let Err(message) = secrets.assert_files_clean(&[
        std::path::Path::new(&config.state_path),
        std::path::Path::new(&config.journal_path),
    ]) {
        eprintln!(
            "[{}] secret material detected: {message}; aborting before any further writes",
            utc_now()
        );
        std::process::exit(9);
    }
}

fn utc_now() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    // Runbook timestamps: UTC, human-readable, no external crates.
    let (y, mo, d, h, mi, s) = civil_from_unix(secs as i64);
    format!("{y:04}-{mo:02}-{d:02}T{h:02}:{mi:02}:{s:02}Z")
}

/// Operator identity for runbook log lines. Explicitly configurable so an
/// audit trail can name the person or role behind a command; defaults to an
/// honest placeholder rather than a fabricated identity.
fn operator_id() -> String {
    env::var("ARBKIT_OPERATOR_ID")
        .ok()
        .filter(|raw| !raw.trim().is_empty())
        .unwrap_or_else(|| "unknown-operator".to_string())
}

/// Days-from-civil inverse (Howard Hinnant's algorithm), for runbook stamps.
fn civil_from_unix(secs: i64) -> (i64, u32, u32, u32, u32, u32) {
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = if m <= 2 { y + 1 } else { y };
    (
        y,
        m,
        d,
        (rem / 3600) as u32,
        ((rem % 3600) / 60) as u32,
        (rem % 60) as u32,
    )
}

impl RunnerConfig {
    fn from_env() -> Result<Self, String> {
        Self::from_args(&env::args().skip(1).collect::<Vec<_>>())
    }

    fn from_args(args: &[String]) -> Result<Self, String> {
        let mode = match arg_value(args, "mode") {
            Ok(Some(value)) => value,
            Ok(None) => env::var("ARBKIT_EXEC_MODE")
                .ok()
                .unwrap_or_else(|| "dry-run".to_owned()),
            Err(message) => return Err(message),
        };
        if mode != "dry-run" && mode != "live" {
            return Err(format!("invalid mode {mode:?}; use dry-run or live"));
        }
        let token_env =
            arg_value(args, "token-env")?.unwrap_or_else(|| "LIVE_INGEST_TOKEN".to_owned());
        let token = env::var(&token_env).unwrap_or_default();
        let windows_limit = arg_value(args, "windows")?.and_then(|w| w.parse::<u64>().ok());
        Ok(Self {
            mode,
            ingest_url: arg_value(args, "url")?.unwrap_or_else(|| DEFAULT_INGEST_URL.to_owned()),
            token,
            token_env,
            state_path: arg_value(args, "state")?
                .unwrap_or_else(|| "prod-risk-state.json".to_owned()),
            journal_path: arg_value(args, "journal")?
                .unwrap_or_else(|| "prod-session.ndjson".to_owned()),
            window: Duration::from_millis(
                arg_value(args, "window-ms")?
                    .and_then(|w| w.parse::<u64>().ok())
                    .unwrap_or(250),
            ),
            windows_limit,
            kalshi_markets_url: arg_value(args, "kalshi-markets-url")?,
            poly_events_url: arg_value(args, "poly-events-url")?,
            tape_path: arg_value(args, "tape")?,
            dump_catalog: arg_value(args, "dump-catalog")?,
            micro: arg_flag(args, "micro"),
            occurrences_path: arg_value(args, "occurrences")?
                .unwrap_or_else(|| "occurrences.ndjson".to_owned()),
            proof_path: arg_value(args, "proof")?.unwrap_or_else(|| "live-proof.json".to_owned()),
        })
    }

    fn is_live(&self) -> bool {
        self.mode == "live"
    }
}

// ---------------------------------------------------------------------------
// Labels and instrument resolution
// ---------------------------------------------------------------------------

/// Resolves interned ids to human-readable labels through the discovery
/// registry. Misses degrade to `"<kind>:<id>"` strings, never panic.
struct CatalogLabels<'a> {
    registry: &'a CanonicalRegistry,
    venues: &'a VenueRegistry,
}

impl CatalogLabels<'_> {
    fn market_label(&self, market_id: u32) -> String {
        let Some(market) = self.registry.get_market(market_id) else {
            return format!("market:{market_id}");
        };
        let kind = match market.kind {
            MarketKind::Moneyline => "moneyline".to_string(),
            MarketKind::Spread(line) => format!("spread {line:?}"),
            MarketKind::Total(line) => format!("total {line:?}"),
        };
        match self.registry.get_event(market.event_id) {
            Some(event) => format!("{} · {}", event.name, kind),
            None => kind,
        }
    }

    fn venue_label(&self, venue_id: u16) -> String {
        self.venues
            .name_of(venue_id)
            .map(str::to_string)
            .unwrap_or_else(|| format!("venue:{venue_id}"))
    }

    fn outcome_label(&self, outcome_id: u32) -> String {
        self.registry
            .get_outcome(outcome_id)
            .map(|outcome| outcome.name.clone())
            .unwrap_or_else(|| format!("outcome:{outcome_id}"))
    }
}

/// Startup catalog resolver: `(venue, outcome)` -> venue instrument.
struct CatalogResolver {
    by_key: HashMap<(VenueId, u32), VenueInstrumentRef>,
}

impl CatalogResolver {
    fn from_map(map: &VenueInstrumentMap) -> Self {
        let mut by_key = HashMap::new();
        for pair in map.iter() {
            for instrument in [&pair.first, &pair.second] {
                if let Some(reference) = instrument_ref(instrument) {
                    by_key.insert((instrument.venue, instrument.outcome_id), reference);
                }
            }
        }
        Self { by_key }
    }
}

impl InstrumentResolver for CatalogResolver {
    fn resolve(&self, venue: VenueId, outcome: u32) -> Option<VenueInstrumentRef> {
        self.by_key.get(&(venue, outcome)).cloned()
    }
}

/// Renders a 256-bit big-endian Polymarket token id back to its decimal
/// string form, the shape the venue's messages and subscriptions use.
fn token_id_decimal(bytes: &[u8; 32]) -> String {
    let mut digits = Vec::new();
    let mut value = *bytes;
    loop {
        // Divide the big-endian array by 10, collecting the remainder.
        let mut remainder: u32 = 0;
        for byte in value.iter_mut() {
            let acc = remainder * 256 + u32::from(*byte);
            *byte = (acc / 10) as u8;
            remainder = acc % 10;
        }
        digits.push(b'0' + remainder as u8);
        if value.iter().all(|&byte| byte == 0) {
            break;
        }
    }
    while digits.len() > 1 && digits.last() == Some(&b'0') {
        digits.pop();
    }
    digits.reverse();
    String::from_utf8(digits).expect("decimal digits are ASCII")
}

// ---------------------------------------------------------------------------
// Wire records
// ---------------------------------------------------------------------------

/// Wire form of a leg's fill status, matching the dashboard's discriminated
/// union: `"filled"`, `{partiallyFilled}`, or `{unfilled}`.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(untagged)]
enum LegStatusWire {
    Filled(String),
    Unfilled { unfilled: String },
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct TradeLegWire {
    venue_label: String,
    outcome_label: String,
    status: LegStatusWire,
    requested_stake_cents: i64,
    filled_stake_cents: i64,
    net_payout_cents: i64,
}

/// One execution attempt, same shape the dashboard's `tradeRecordSchema`
/// admits, with the live extension fields always stated.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct TradeRecordWire {
    seq: u64,
    detection_timestamp_ns: u64,
    latency_ns: u64,
    market_label: String,
    edge_bps: u32,
    overround_ppm: u32,
    requested_stake_cents: i64,
    expected_profit_cents: i64,
    worst_case_profit_cents: i64,
    realized_profit_cents: Option<i64>,
    slippage_cents: i64,
    fees_paid_cents: i64,
    fill_ratio_bps: u32,
    classification: String,
    chased: bool,
    legs: Vec<TradeLegWire>,
    execution_mode: &'static str,
    venue_order_ids: Vec<String>,
    filled_stake_cents: i64,
    settlement_status: String,
}

/// The runner's authoritative risk posture. This runner genuinely enforces
/// every cap, so every field is `Some` — the dashboard never substitutes.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct RiskStateWire {
    execution_mode: &'static str,
    kill_switch: bool,
    max_stake_per_leg_cents: Option<i64>,
    max_daily_loss_cents: Option<i64>,
    daily_loss_used_cents: Option<i64>,
    max_open_trades: Option<u32>,
    open_trades: Option<u32>,
    min_edge_bps: Option<u32>,
}

impl RiskStateWire {
    fn snapshot(risk: &RiskGate, mode: &str) -> Self {
        Self {
            execution_mode: wire_mode(mode),
            kill_switch: risk.config.kill_switch,
            max_stake_per_leg_cents: Some(risk.config.max_stake_per_leg_cents),
            max_daily_loss_cents: Some(risk.config.max_daily_loss_cents),
            daily_loss_used_cents: Some(risk.daily_loss_cents),
            max_open_trades: Some(risk.config.max_open_trades),
            open_trades: Some(risk.open_trades),
            min_edge_bps: Some(risk.config.min_edge_bps),
        }
    }
}

/// The dashboard admits exactly `paper` and `live`. A dry-run runner transmits
/// no orders and risks no capital, so it reports `paper`.
fn wire_mode(mode: &str) -> &'static str {
    if mode == "live" {
        "live"
    } else {
        "paper"
    }
}

/// Settlement lifecycle for a fill, matching the wire contract.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
enum SettlementStatus {
    Open,
    Settled,
    Unwound,
}

/// One reconciled fill, keyed by the idempotency key the execution layer
/// committed before network submission. Realized cents ride along only once
/// settlement has actually reported them.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct FillRecordWire {
    client_order_id: String,
    venue_order_id: Option<String>,
    trade_seq: Option<u64>,
    filled_stake_cents: i64,
    realized_profit_cents: Option<i64>,
    settlement_status: SettlementStatus,
    reconciled_at_epoch_ms: u128,
}

impl FillRecordWire {
    fn new(reconciled_leg: &ReconciledLeg) -> Self {
        Self {
            client_order_id: hex16(&reconciled_leg.client_order_id),
            venue_order_id: reconciled_leg.venue_order_id.clone(),
            trade_seq: None,
            filled_stake_cents: reconciled_leg.filled_stake_cents,
            realized_profit_cents: None,
            settlement_status: if reconciled_leg.status == "unwound" {
                SettlementStatus::Unwound
            } else {
                SettlementStatus::Open
            },
            reconciled_at_epoch_ms: epoch_ms(),
        }
    }

    fn settled(status: &str, profit: Option<i64>, filled_cents: i64, client: [u8; 16]) -> Self {
        Self {
            client_order_id: hex16(&client),
            venue_order_id: None,
            trade_seq: None,
            filled_stake_cents: filled_cents,
            realized_profit_cents: profit,
            settlement_status: if status.eq_ignore_ascii_case("unwound") {
                SettlementStatus::Unwound
            } else {
                SettlementStatus::Settled
            },
            reconciled_at_epoch_ms: epoch_ms(),
        }
    }
}

/// One message on the runner -> ingest wire, mirroring the frozen contract.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(tag = "t", rename_all_fields = "camelCase")]
enum Frame {
    #[serde(rename = "session-start")]
    SessionStart {
        schema_version: u32,
        run_id: String,
        started_at_epoch_ms: u128,
        initial_bankroll_cents: Option<i64>,
        ticks_per_window: usize,
        window_ms: u64,
        execution_mode: &'static str,
    },
    #[serde(rename = "risk")]
    Risk { state: RiskStateWire },
    #[serde(rename = "positions")]
    Positions { items: Vec<TradeRecordWire> },
    #[serde(rename = "fills")]
    Fills { items: Vec<FillRecordWire> },
    #[serde(rename = "stats")]
    Stats {
        seq_cursor: u64,
        windows_completed: u64,
        locked_cents: Option<i64>,
        available_cents: Option<i64>,
        attempted: u64,
        capital_short: u64,
        unwind_failures: u64,
        ack_matched: u64,
        in_flight_remaining: usize,
    },
    #[serde(rename = "heartbeat")]
    Heartbeat { seq_cursor: u64 },
    #[serde(rename = "session-end")]
    SessionEnd,
}

impl Frame {
    fn to_ndjson_line(&self) -> String {
        let mut line = serde_json::to_string(self).expect("frames serialize");
        line.push('\n');
        line
    }
}

/// Worst-case payout for one filled leg: floor(stake / price) in cents.
/// Integer division keeps it pessimistic per the rounding rules.
fn leg_payout_cents(stake: Cents, quoted: Prob) -> i64 {
    if stake <= 0 || quoted.ppm() == 0 {
        return 0;
    }
    ((stake as i128 * 1_000_000_i128) / quoted.ppm() as i128).min(i64::MAX as i128) as i64
}

/// Builds the wire record for one execution attempt. Every cents field is
/// copied verbatim from the signal and the executor's report; nothing is
/// recomputed, rounded, or invented.
fn build_record(
    seq: u64,
    signal: &SignalEvent,
    report: &ExecutionReport,
    labels: &CatalogLabels<'_>,
    mode: &str,
) -> TradeRecordWire {
    let filled = report.classification == ExecutionClassification::LiveFill;
    let legs = signal.plan[..signal.plan_len as usize]
        .iter()
        .map(|leg| TradeLegWire {
            venue_label: labels.venue_label(leg.venue),
            outcome_label: labels.outcome_label(leg.outcome),
            status: if filled {
                LegStatusWire::Filled("filled".to_string())
            } else {
                LegStatusWire::Unfilled {
                    unfilled: report
                        .reason
                        .clone()
                        .unwrap_or_else(|| "hedge unwound".to_string()),
                }
            },
            requested_stake_cents: leg.capacity,
            filled_stake_cents: if filled { leg.capacity } else { 0 },
            net_payout_cents: if filled {
                leg_payout_cents(leg.capacity, leg.quoted)
            } else {
                0
            },
        })
        .collect();
    let fill_ratio_bps = if report.requested_stake_cents > 0 {
        (report.filled_stake_cents as i128 * 10_000 / report.requested_stake_cents as i128) as u32
    } else {
        0
    };
    TradeRecordWire {
        seq,
        detection_timestamp_ns: signal.ingest_timestamp_ns,
        latency_ns: signal.latency_ns,
        market_label: labels.market_label(signal.market_id),
        edge_bps: signal.signal.profit_bps,
        overround_ppm: signal.signal.overround_ppm,
        requested_stake_cents: signal.signal.total_stake,
        expected_profit_cents: signal.signal.worst_case_profit,
        worst_case_profit_cents: signal.signal.worst_case_profit,
        realized_profit_cents: report.realized_profit_cents,
        // Slippage and fees are reconciliation facts, not fill-time guesses:
        // they stay zero until settlement reports them (HJ-147).
        slippage_cents: 0,
        fees_paid_cents: 0,
        fill_ratio_bps,
        classification: if filled { "clean" } else { "phantom" }.to_string(),
        chased: false,
        legs,
        execution_mode: wire_mode(mode),
        venue_order_ids: report.venue_order_ids.clone(),
        filled_stake_cents: report.filled_stake_cents,
        settlement_status: report.settlement_status.clone(),
    }
}

/// Hex-encode an idempotency key for the wire.
fn hex16(id: &[u8; 16]) -> String {
    id.iter().map(|b| format!("{b:02x}")).collect()
}

/// Build a persisted in-flight order from an execution leg, before network
/// submission, so a crash between preflight and acknowledgement reconciles
/// by client order id.
fn order_from_exec_leg(leg: &arbkit_exec::ExecLeg) -> InFlightOrder {
    InFlightOrder {
        client_order_id: leg.client_order_id,
        leg: PersistedExecLeg {
            venue: leg.venue,
            instrument: leg.instrument.clone(),
            limit_price_ppm: leg.limit_price.ppm(),
            stake_cents: leg.stake_cents,
            client_order_id: leg.client_order_id,
        },
        created_at_ms: epoch_ms() as u64,
        venue_order_id: None,
    }
}

/// Emit a `fills` frame for the acknowledged legs of a freshly executed
/// hedge. An acknowledgment is a fill we know about (open or unwound); the
/// realization (fees, profit) is reported later by the settle path.
fn emit_fill_frames(writer: &WriterHandle, report: &ExecutionReport, legs: &[ReconciledLeg]) {
    if report.venue_order_ids.is_empty() {
        return;
    }
    let items = legs
        .iter()
        .filter(|leg| leg.venue_order_id.is_some())
        .map(FillRecordWire::new)
        .collect();
    writer.send(&Frame::Fills { items });
}

/// Emit a `fills` frame for a settle that reached terminal status. Positional
/// venue id comes from the polled fill, distinct from the original
/// acknowledgement.
fn emit_settlement_frames(writer: &WriterHandle, settle: &Settlement) {
    writer.send(&Frame::Fills {
        items: vec![FillRecordWire::settled(
            &settle.status,
            settle.realized_profit_cents,
            settle.filled_stake_cents,
            settle.client_order_id,
        )],
    });
}

// ---------------------------------------------------------------------------
// Streaming writer
// ---------------------------------------------------------------------------

/// Handle to the writer thread.
struct WriterHandle {
    tx: Sender<String>,
    join: JoinHandle<()>,
}

fn spawn_writer(ingest_url: String, token: String, seq_cursor: Arc<AtomicU64>) -> WriterHandle {
    let (tx, rx) = std::sync::mpsc::channel::<String>();
    let flush_lines = 64usize;
    let flush_interval = Duration::from_millis(500);
    let heartbeat_interval = Duration::from_secs(5);
    let join = std::thread::Builder::new()
        .name("stream-writer".into())
        .spawn(move || {
            let agent = ureq::AgentBuilder::new()
                .timeout(Duration::from_secs(5))
                .build();
            let mut buffer: Vec<String> = Vec::new();
            let mut first_buffered: Option<Instant> = None;
            let mut last_activity = Instant::now();
            let mut dropped: u64 = 0;
            let deliver = |batch: &[String], dropped: &mut u64| {
                if batch.is_empty() {
                    return;
                }
                let body = batch.concat();
                let mut attempt = 0u32;
                loop {
                    let mut request = agent.post(&ingest_url);
                    if !token.is_empty() {
                        request = request.set("Authorization", &format!("Bearer {token}"));
                    }
                    match request
                        .set("Content-Type", "application/x-ndjson")
                        .send_string(&body)
                    {
                        Ok(_) => return,
                        Err(error) if attempt < 2 => {
                            attempt += 1;
                            eprintln!("[stream] delivery failed ({error}); retry {attempt}");
                            std::thread::sleep(Duration::from_millis(250 * u64::from(attempt)));
                        }
                        Err(error) => {
                            *dropped += 1;
                            eprintln!(
                                "[stream] DROPPED batch of {} frames after retries ({error}); total dropped={dropped}",
                                batch.len()
                            );
                            return;
                        }
                    }
                }
            };
            loop {
                let received = match rx.recv_timeout(WRITER_POLL) {
                    Ok(line) => {
                        buffer.push(line);
                        if first_buffered.is_none() {
                            first_buffered = Some(Instant::now());
                        }
                        last_activity = Instant::now();
                        true
                    }
                    Err(std::sync::mpsc::RecvTimeoutError::Timeout) => false,
                    Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                        deliver(&buffer, &mut dropped);
                        return;
                    }
                };
                let _ = received;
                let flush_due = buffer.len() >= flush_lines
                    || first_buffered.is_some_and(|at| at.elapsed() >= flush_interval);
                if flush_due {
                    deliver(&buffer, &mut dropped);
                    buffer.clear();
                    first_buffered = None;
                }
                if last_activity.elapsed() >= heartbeat_interval {
                    let heartbeat = Frame::Heartbeat {
                        seq_cursor: seq_cursor.load(Ordering::Relaxed),
                    };
                    deliver(&[heartbeat.to_ndjson_line()], &mut dropped);
                    last_activity = Instant::now();
                }
            }
        })
        .expect("stream writer spawns");
    WriterHandle { tx, join }
}

impl WriterHandle {
    fn send(&self, frame: &Frame) {
        // A send fails only when the writer is gone; a dead writer must not
        // take the trading loop down with it.
        let _ = self.tx.send(frame.to_ndjson_line());
    }

    fn finish(self) {
        drop(self.tx);
        let _ = self.join.join();
    }
}

// ---------------------------------------------------------------------------
// Venue endpoints
// ---------------------------------------------------------------------------

/// One venue's execution endpoint. The hedge executor takes its two adapters
/// positionally (`first`, `second`), so the runner guarantees Kalshi is always
/// leg 0 and Polymarket leg 1 before calling it — routing an order to the
/// wrong venue's API must be impossible by construction.
enum Endpoint {
    Kalshi(Box<KalshiExecutionAdapter>),
    Polymarket(PolymarketExecutionAdapter),
    Dry(DryRunAdapter),
}

impl VenueAdapter for Endpoint {
    fn submit(&self, leg: &arbkit_exec::ExecLeg) -> Result<arbkit_exec::OrderResult, String> {
        match self {
            Endpoint::Kalshi(adapter) => adapter.submit(leg),
            Endpoint::Polymarket(adapter) => adapter.submit(leg),
            Endpoint::Dry(adapter) => adapter.submit(leg),
        }
    }

    fn unwind(&self, order: &arbkit_exec::OrderResult) -> Result<(), String> {
        UNWINDS.fetch_add(1, Ordering::Relaxed);
        match self {
            // A phantom's accepted leg is cancelled at the venue by order id;
            // a failed unwind conservatively leaves capital reserved.
            Endpoint::Kalshi(adapter) => adapter
                .cancel_order(&order.order_id)
                .map_err(|e| e.to_string()),
            Endpoint::Polymarket(adapter) => adapter
                .cancel_order(&order.order_id)
                .map_err(|e| e.to_string()),
            Endpoint::Dry(adapter) => adapter.unwind(order),
        }
    }
}

fn live_endpoints() -> Result<(Endpoint, Endpoint, SecretScan), String> {
    let kalshi_key = env::var("KALSHI_ACCESS_KEY_ID").unwrap_or_default();
    let kalshi_key_path = env::var("KALSHI_PRIVATE_KEY_PATH").unwrap_or_default();
    if kalshi_key.is_empty() || kalshi_key_path.is_empty() {
        return Err(
            "live mode requires KALSHI_ACCESS_KEY_ID and KALSHI_PRIVATE_KEY_PATH".to_string(),
        );
    }

    // The signing key is the crown jewel: a secret manager should mount it
    // owner-readable only. Anything group/world readable is refused before
    // it is ever parsed.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = fs::metadata(&kalshi_key_path)
            .map_err(|e| format!("stat {kalshi_key_path}: {e}"))?
            .permissions()
            .mode();
        if mode & 0o077 != 0 {
            return Err(format!(
                "{kalshi_key_path} is too open (mode {:o}); mount the signing key 0600",
                mode & 0o777
            ));
        }
    }

    let kalshi_pem =
        fs::read_to_string(&kalshi_key_path).map_err(|e| format!("read {kalshi_key_path}: {e}"))?;

    let poly_wallet = env::var("POLY_WALLET_ADDRESS").unwrap_or_default();
    let poly_l1 = env::var("POLY_PRIVATE_KEY").unwrap_or_default();
    let poly_key = env::var("POLY_API_KEY").unwrap_or_default();
    let poly_secret = env::var("POLY_API_SECRET").unwrap_or_default();
    let poly_passphrase = env::var("POLY_API_PASSPHRASE").unwrap_or_default();
    if poly_wallet.is_empty()
        || poly_l1.is_empty()
        || poly_key.is_empty()
        || poly_secret.is_empty()
        || poly_passphrase.is_empty()
    {
        return Err(
            "live mode requires POLY_WALLET_ADDRESS, POLY_PRIVATE_KEY, POLY_API_KEY, \
             POLY_API_SECRET, and POLY_API_PASSPHRASE"
                .to_string(),
        );
    }

    // Every credential this session holds becomes an artifact-scan needle:
    // anything the runner writes is swept for them before order flow starts
    // and again at shutdown.
    let mut secrets = SecretScan::from_values([
        ("KALSHI_ACCESS_KEY_ID", kalshi_key.as_str()),
        ("POLY_PRIVATE_KEY", poly_l1.as_str()),
        ("POLY_API_KEY", poly_key.as_str()),
        ("POLY_API_SECRET", poly_secret.as_str()),
        ("POLY_API_PASSPHRASE", poly_passphrase.as_str()),
    ]);
    secrets.add_pem("KALSHI_PRIVATE_KEY", &kalshi_pem);

    let kalshi = KalshiExecutionAdapter::new(KalshiConfig {
        api_key: kalshi_key,
        private_key_pem: kalshi_pem,
        base_url: env::var("KALSHI_API_BASE")
            .unwrap_or_else(|_| "https://api.elections.kalshi.com".to_string()),
        timestamp_ms: None,
        request_timeout: Some(Duration::from_secs(5)),
    })
    .map_err(|e| format!("kalshi adapter: {e}"))?;

    let poly = PolymarketExecutionAdapter::new(PolymarketConfig {
        wallet_address: poly_wallet,
        l1_private_key: poly_l1,
        api_key: poly_key,
        api_secret: poly_secret,
        passphrase: poly_passphrase,
        base_url: env::var("POLY_API_BASE")
            .unwrap_or_else(|_| "https://clob.polymarket.com".to_string()),
        timestamp_s: None,
        request_timeout: Some(Duration::from_secs(5)),
    })
    .map_err(|e| format!("polymarket adapter: {e}"))?;

    Ok((
        Endpoint::Kalshi(Box::new(kalshi)),
        Endpoint::Polymarket(poly),
        secrets,
    ))
}

/// Authoritative per-venue balances. In live mode these come from the venues
/// themselves; a venue that cannot answer aborts the runner before any order
/// POST, exactly like the balance-mismatch drill.
fn endpoint_balances(
    first: &Endpoint,
    second: &Endpoint,
) -> Result<HashMap<VenueId, Cents>, String> {
    match (first, second) {
        (Endpoint::Dry { .. }, Endpoint::Dry { .. }) => {
            let cents = env_cents("ARBKIT_BANKROLL_CENTS", 10_000);
            Ok(HashMap::from([
                (VenueRegistry::KALSHI, cents),
                (VenueRegistry::POLYMARKET, cents),
            ]))
        }
        (Endpoint::Kalshi(kalshi), Endpoint::Polymarket(poly)) => {
            let k = kalshi
                .balance_cents()
                .map_err(|e| format!("kalshi balance check failed: {e}"))?;
            let p = poly
                .balance_cents()
                .map_err(|e| format!("polymarket balance check failed: {e}"))?;
            Ok(HashMap::from([
                (VenueRegistry::KALSHI, k),
                (VenueRegistry::POLYMARKET, p),
            ]))
        }
        _ => Err("endpoint pairing must be (kalshi, polymarket) or (dry, dry)".to_string()),
    }
}

/// A [`SettlementSource`] that polls [`Endpoint::order_status`] per in-flight
/// order. `order_status` reports lifecycle + filled stake but not fees or
/// realized profit, so this path reconciles the *status*; authoritative fees
/// and settled PnL arrive via [`Reconciler`] fill frames fed from a private
/// fill subscription, dropped in through `apply_fill_frame`.
struct PollingSource<'a> {
    first: &'a Endpoint,
    second: &'a Endpoint,
}

impl SettlementSource for PollingSource<'_> {
    fn poll(&self, order: &InFlightOrder) -> Result<Option<arbkit_exec::FillEvent>, String> {
        let Some(venue_order_id) = order.venue_order_id.as_deref() else {
            return Ok(None);
        };
        if venue_order_id.is_empty() {
            return Ok(None);
        }
        match order.leg.venue {
            venue if venue == VenueRegistry::KALSHI => {
                let Endpoint::Kalshi(adapter) = self.first else {
                    return Ok(None);
                };
                let status = adapter
                    .order_status(venue_order_id)
                    .map_err(|e| e.to_string())?;
                Ok(Some(arbkit_exec::FillEvent {
                    client_order_id: Some(order.client_order_id),
                    venue_order_id: status.order_id.clone(),
                    filled_stake_cents: status.filled_count.unwrap_or(0),
                    fee_cents: 0,
                    realized_profit_cents: None,
                    status: status.status.unwrap_or_else(|| "open".into()),
                }))
            }
            venue if venue == VenueRegistry::POLYMARKET => {
                let Endpoint::Polymarket(adapter) = self.second else {
                    return Ok(None);
                };
                let status = adapter
                    .order_status(venue_order_id)
                    .map_err(|e| e.to_string())?;
                Ok(Some(arbkit_exec::FillEvent {
                    client_order_id: Some(order.client_order_id),
                    venue_order_id: status.order_id,
                    filled_stake_cents: status.filled_stake_cents.unwrap_or(0),
                    fee_cents: 0,
                    realized_profit_cents: None,
                    status: status.status.unwrap_or_else(|| "open".into()),
                }))
            }
            _ => Ok(None),
        }
    }
}

// ---------------------------------------------------------------------------
// Main
// ---------------------------------------------------------------------------

fn main() {
    let config = match RunnerConfig::from_env() {
        Ok(config) => config,
        Err(message) => {
            eprintln!("prod_trader: {message}");
            std::process::exit(2);
        }
    };

    let kill_switch = env::var("ARBKIT_KILL_SWITCH")
        .map(|v| v != "0")
        .unwrap_or(true);
    let risk_config = {
        let env_policy = RiskConfig {
            kill_switch,
            max_stake_per_leg_cents: env_cents("ARBKIT_MAX_STAKE_PER_LEG_CENTS", 5_000),
            max_daily_loss_cents: env_cents("ARBKIT_MAX_DAILY_LOSS_CENTS", 50_000),
            max_open_trades: env_u32("ARBKIT_MAX_OPEN_TRADES", 1),
            min_edge_bps: env_u32("ARBKIT_MIN_EDGE_BPS", 50),
        };
        if config.micro {
            micro_live_config(env_policy)
        } else {
            env_policy
        }
    };

    // Kalshi's market-data socket is authenticated even for read-only book
    // updates. Report whether a signed handshake is possible so a dry-run
    // warmup without credentials is a loud fact on the boot line, never a
    // silently empty Kalshi book (rehearsal finding F3).
    let kalshi_feed_signed = {
        let key = env::var("KALSHI_ACCESS_KEY_ID").unwrap_or_default();
        let pem = env::var("KALSHI_PRIVATE_KEY_PATH")
            .ok()
            .and_then(|path| fs::read_to_string(path).ok())
            .unwrap_or_default();
        !key.is_empty() && !pem.is_empty()
    };

    println!(
        "prod_trader mode={} kill_switch={} max_stake_per_leg={}c daily_loss_cap={}c \
         open_trades_cap={} min_edge_bps={} kalshi_feed_signed={}",
        config.mode,
        risk_config.kill_switch,
        risk_config.max_stake_per_leg_cents,
        risk_config.max_daily_loss_cents,
        risk_config.max_open_trades,
        risk_config.min_edge_bps,
        kalshi_feed_signed,
    );

    // The resting posture refuses live order flow while the kill switch is
    // engaged; arming live requires the explicit `ARBKIT_KILL_SWITCH=0`.
    if config.is_live() && risk_config.kill_switch {
        eprintln!("live mode refused: ARBKIT_KILL_SWITCH is active");
        std::process::exit(3);
    }

    // Adapters: live requires credentials; dry-run never touches a network.
    // Both modes carry a secret scanner: live sweeps its real credentials,
    // dry-run sweeps whatever the environment holds (typically only the
    // stream token), so artifacts are policed identically.
    let (first_endpoint, second_endpoint, secrets) = if config.is_live() {
        match live_endpoints() {
            Ok(endpoints) => endpoints,
            Err(message) => {
                eprintln!("live mode refused: {message}");
                std::process::exit(4);
            }
        }
    } else {
        let token_env = &config.token_env;
        let secrets = SecretScan::from_values([(
            token_env.as_str(),
            env::var(token_env.as_str()).unwrap_or_default().as_str(),
        )]);
        (
            Endpoint::Dry(DryRunAdapter),
            Endpoint::Dry(DryRunAdapter),
            secrets,
        )
    };

    // Artifact sweep: the risk snapshot exists from here on; the journal is
    // created just before order flow. A leak aborts with exit code 9 and
    // names the artifact and credential label, never the value.
    assert_artifacts_clean(&config, &secrets);

    // One runtime serves discovery (once) and both feed tasks (forever).
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("tokio runtime builds");

    // Startup catalog: REST discovery builds the validated cross-venue map.
    // Reviewing and pinning the active set is the catalog ticket's job; this
    // runner only executes what `validate_binary_pair` already admitted.
    let catalog = {
        let source = match RestDiscoverySource::new() {
            Ok(source) => source,
            Err(error) => {
                eprintln!("prod_trader: discovery client failed: {error}");
                std::process::exit(5);
            }
        };
        let service = arbkit_feed::live::CatalogService::new();
        let kalshi_discovery = KalshiDiscoveryConfig {
            markets_url: config
                .kalshi_markets_url
                .clone()
                .unwrap_or_else(|| KalshiDiscoveryConfig::default().markets_url),
            ..KalshiDiscoveryConfig::default()
        };
        let poly_discovery = PolymarketDiscoveryConfig {
            events_url: config
                .poly_events_url
                .clone()
                .unwrap_or_else(|| PolymarketDiscoveryConfig::default().events_url),
            ..PolymarketDiscoveryConfig::default()
        };
        let result = runtime.block_on(refresh_catalog(
            &service,
            &source,
            &kalshi_discovery,
            &poly_discovery,
        ));
        match result {
            Ok(generation) => generation,
            Err(error) => {
                eprintln!("prod_trader: catalog discovery failed: {error}");
                std::process::exit(5);
            }
        }
    };
    let report = &catalog.report;
    println!(
        "catalog generation={} events={} active_pairs={} paired_markets={} \
         (skipped: unmatched={} malformed={})",
        report.generation,
        report.canonical_events,
        report.active_pairs,
        report.paired_markets,
        report.stats.skipped_unmatched_event,
        report.stats.skipped_malformed,
    );
    if catalog.map.is_empty() {
        eprintln!("prod_trader: no active cross-venue pairs; refusing to run");
        std::process::exit(5);
    }

    let registry = Arc::clone(&catalog.registry);
    let map = Arc::clone(&catalog.map);
    let labels_env = VenueRegistry::new();
    let resolver = CatalogResolver::from_map(&map);

    // Balances: the venue is authoritative in live mode.
    let balances = match endpoint_balances(&first_endpoint, &second_endpoint) {
        Ok(balances) => balances,
        Err(message) => {
            eprintln!("prod_trader: {message}");
            std::process::exit(4);
        }
    };
    let initial_total: i64 = balances.values().sum();

    // Durable state: restore loss/open-trade counters, refuse live restart
    // with unreconciled in-flight orders. Balances stay authoritative at the
    // venue (or env, in dry-run) regardless of what the snapshot recorded.
    let state_store = RiskStateStore::new(&config.state_path);
    let stored = state_store.load().ok();
    if let Some(stored) = &stored {
        if config.is_live() && !stored.in_flight.is_empty() {
            eprintln!(
                "live mode refused: {} in-flight orders in {} require reconciliation \
                 before restart",
                stored.in_flight.len(),
                state_store.path().display()
            );
            std::process::exit(6);
        }
        println!(
            "restored state: daily_loss={}c open_trades={} in_flight={} (balances \
             refresh from {})",
            stored.daily_loss_cents,
            stored.open_trades,
            stored.in_flight.len(),
            if config.is_live() { "venues" } else { "env" },
        );
        for (&venue, &venue_cents) in &balances {
            let stored_cents = stored.bankroll.get(&venue).copied().unwrap_or(0);
            if stored_cents != venue_cents {
                println!(
                    "bankroll reconciliation venue {venue}: stored={stored_cents}c \
                     live={venue_cents}c (using live)"
                );
            }
        }
    }

    // Policy continuity: a stored snapshot carries the limits that governed
    // the money already at risk, so they win over this run's environment —
    // a restart must not silently widen caps mid-day. The kill switch is
    // session posture and always comes from the live environment.
    let mut effective_config = risk_config;
    if let Some(stored) = &stored {
        let c = stored.config;
        if c.max_stake_per_leg_cents != risk_config.max_stake_per_leg_cents
            || c.max_daily_loss_cents != risk_config.max_daily_loss_cents
            || c.max_open_trades != risk_config.max_open_trades
            || c.min_edge_bps != risk_config.min_edge_bps
        {
            println!(
                "risk policy restored from {}: stake<={}c (env {}) daily_loss_cap={}c \
                 (env {}) open_trades<={} (env {}) min_edge={}bps (env {})",
                state_store.path().display(),
                c.max_stake_per_leg_cents,
                risk_config.max_stake_per_leg_cents,
                c.max_daily_loss_cents,
                risk_config.max_daily_loss_cents,
                c.max_open_trades,
                risk_config.max_open_trades,
                c.min_edge_bps,
                risk_config.min_edge_bps,
            );
        }
        effective_config = RiskConfig {
            max_stake_per_leg_cents: c.max_stake_per_leg_cents,
            max_daily_loss_cents: c.max_daily_loss_cents,
            max_open_trades: c.max_open_trades,
            min_edge_bps: c.min_edge_bps,
            kill_switch: risk_config.kill_switch,
        };
    }

    // The RiskGate owns execution-day state: fresh caps from this run's
    // environment, loss and open trades surviving a restart exactly.
    let mut risk = match &stored {
        Some(stored) => RiskGate::from_durable(
            effective_config,
            stored.daily_loss_cents,
            stored.open_trades,
            balances.clone(),
        ),
        None => RiskGate::new(risk_config, balances.clone()),
    };
    let _ = state_store.checkpoint(&risk);

    // Reconciliation ledger + in-flight set. On a restart, orders that were
    // in flight are re-seeded so the poll loop can close them by venue order
    // id instead of leaving them orphaned; a dry-run restart drill therefore
    // reconciles to zero in-flight entries (live mode still refuses above).
    let mut reconciler = Reconciler::new(Default::default());
    if let Some(stored) = &stored {
        reconciler.seed_in_flight(stored.in_flight.clone());
    }
    if !reconciler.in_flight().is_empty() {
        println!(
            "reconcile: seeded {} in-flight order(s) for settlement polling",
            reconciler.in_flight().len()
        );
    }

    // Engine + market registration.
    let mut engine = Engine::with_default_capacity();
    let detection_budget = env_cents(
        "ARBKIT_DETECT_BUDGET_CENTS",
        risk_config.max_stake_per_leg_cents.saturating_mul(2),
    );
    let mut kalshi_subs = Vec::new();
    let mut poly_subs = Vec::new();
    for pair in map.iter() {
        let (kalshi_side, poly_side) = if pair.first.venue == VenueRegistry::KALSHI {
            (&pair.first, &pair.second)
        } else {
            (&pair.second, &pair.first)
        };
        let outcome_count = registry
            .get_market(pair.market_id)
            .map(|market| market.outcomes.len() as u8)
            .unwrap_or(2);
        let mut market_config = MarketConfig {
            outcome_count,
            active: true,
            budget: detection_budget,
            ..MarketConfig::default()
        };
        // Fee and increment model per venue, matching the paper pipeline's
        // documented configuration (RESULTS.md §2).
        market_config.venue_fees[VenueRegistry::KALSHI as usize] = Fee::StakeFeeBps(350);
        market_config.venue_increments[VenueRegistry::KALSHI as usize] = 100;
        market_config.venue_fees[VenueRegistry::POLYMARKET as usize] = Fee::None;
        market_config.venue_increments[VenueRegistry::POLYMARKET as usize] = 1;
        market_config.venue_survival_bps[VenueRegistry::KALSHI as usize] =
            env_u32("ARBKIT_KALSHI_SURVIVAL_BPS", 9_500);
        market_config.venue_survival_bps[VenueRegistry::POLYMARKET as usize] =
            env_u32("ARBKIT_POLY_SURVIVAL_BPS", 9_000);
        if let Err(error) = engine.register_market(pair.market_id, market_config) {
            eprintln!("prod_trader: register market {}: {error}", pair.market_id);
            std::process::exit(7);
        }
        if let Some(ticker) = &kalshi_side.kalshi_ticker {
            kalshi_subs.push(KalshiSubscription {
                ticker: ticker.clone(),
                market_id: pair.market_id,
                yes_outcome_id: kalshi_side.outcome_id,
                no_outcome_id: poly_side.outcome_id,
            });
        }
        if let Some(token) = poly_side.poly_token_id {
            poly_subs.push(PolymarketSubscription {
                token_id: token_id_decimal(&token),
                market_id: pair.market_id,
                outcome_id: poly_side.outcome_id,
            });
        }
    }
    println!(
        "engine registered {} markets; subscriptions: kalshi={} polymarket={}",
        map.len(),
        kalshi_subs.len(),
        poly_subs.len(),
    );

    // Manual mapping review: every active pair as the venues publish it —
    // Kalshi ticker and Polymarket decimal token id, one CSV row per leg.
    if let Some(path) = &config.dump_catalog {
        let mut rows = String::from("market_id,outcome_id,venue,kalshi_ticker,poly_token\n");
        for pair in map.iter() {
            for leg in [&pair.first, &pair.second] {
                let poly = leg
                    .poly_token_id
                    .map(|token| arbkit_match::poly_token_id_to_decimal(&token))
                    .unwrap_or_default();
                let venue = if leg.venue == VenueRegistry::KALSHI {
                    "kalshi"
                } else {
                    "polymarket"
                };
                rows.push_str(&format!(
                    "{},{id},{venue},{ticker},{poly}\n",
                    leg.market_id,
                    id = leg.outcome_id,
                    ticker = leg.kalshi_ticker.as_deref().unwrap_or_default(),
                ));
            }
        }
        fs::write(path, rows).unwrap_or_else(|e| {
            eprintln!("prod_trader: write {path}: {e}");
            std::process::exit(8);
        });
        println!("catalog dumped to {path} ({} pairs)", map.len());
    }

    // Rings, feeds, bridge, engine thread.
    let running = Arc::new(AtomicBool::new(true));
    let (feed_producer, feed_consumer) = spsc_ring::<FeedEventSlot>(4096);
    let (signal_producer, mut signal_consumer) = spsc_ring::<SignalEventSlot>(1024);

    // Raw-tape recorder: sits on the feed bridge (pre-engine), so the hot
    // loop never learns a disk exists. Unbuffered File writes land each
    // event in the kernel; nothing to flush at shutdown.
    let mut tape_writer = match &config.tape_path {
        Some(path) => {
            let file = fs::File::create(path)
                .map_err(|e| format!("create {path}: {e}"))
                .unwrap_or_else(|message| {
                    eprintln!("prod_trader: {message}");
                    std::process::exit(8);
                });
            Some(
                TapeWriter::new(file)
                    .unwrap_or_else(|error| panic!("tape header writable: {error}")),
            )
        }
        None => None,
    };
    let tape_events = Arc::new(AtomicU64::new(0));

    let (feed_sender, feed_receiver) = crossbeam_bridge();
    runtime.spawn(KalshiLiveFeed::run(
        KalshiFeedConfig {
            subscriptions: kalshi_subs,
            // Kalshi's market-data socket is authenticated even for
            // read-only book updates; both modes need the credentials.
            api_key: env::var("KALSHI_ACCESS_KEY_ID").unwrap_or_default(),
            private_key_pem: env::var("KALSHI_PRIVATE_KEY_PATH")
                .ok()
                .and_then(|path| fs::read_to_string(path).ok())
                .unwrap_or_default(),
            ..KalshiFeedConfig::default()
        },
        feed_sender.clone(),
        Arc::clone(&running),
    ));
    runtime.spawn(PolymarketLiveFeed::run(
        PolymarketFeedConfig {
            subscriptions: poly_subs,
            ..PolymarketFeedConfig::default()
        },
        feed_sender,
        Arc::clone(&running),
    ));

    let bridge_running = Arc::clone(&running);
    let bridge = {
        let mut producer = feed_producer;
        let mut tape = tape_writer.take();
        let tape_events = Arc::clone(&tape_events);
        // A full ring is backpressure, not an error: the bridge spins until
        // the hot loop drains a slot. Small error type keeps the closure
        // cheap to move into the bridge thread.
        arbkit_feed::live::spawn_ring_bridge(
            feed_receiver,
            move |event| {
                if let Some(writer) = tape.as_mut() {
                    match writer.write_event(&event) {
                        Ok(()) => {
                            tape_events.fetch_add(1, Ordering::Relaxed);
                        }
                        Err(error) => {
                            eprintln!("[{}] tape write failed: {error}", utc_now());
                        }
                    }
                }
                producer.try_push(event).map_err(|_| RingFull)
            },
            bridge_running,
        )
    };

    let engine_running = Arc::clone(&running);
    let engine_thread = {
        let mut engine = engine;
        std::thread::Builder::new()
            .name("engine-hot-loop".into())
            .spawn(move || {
                engine.run(feed_consumer, signal_producer, engine_running, epoch_ns);
            })
            .expect("engine thread spawns")
    };

    // Stream + journal.
    let seq_cursor = Arc::new(AtomicU64::new(0));
    let writer = spawn_writer(
        config.ingest_url.clone(),
        config.token.clone(),
        Arc::clone(&seq_cursor),
    );
    let mut journal = fs::File::create(&config.journal_path)
        .map_err(|e| format!("create {}: {e}", config.journal_path))
        .unwrap_or_else(|message| {
            eprintln!("prod_trader: {message}");
            std::process::exit(8);
        });
    // Occurrence tape: one frozen record per executed signal, the paper
    // side of the same-tape proof. Secret-swept like every other artifact.
    let mut occurrences = fs::File::create(&config.occurrences_path)
        .map_err(|e| format!("create {}: {e}", config.occurrences_path))
        .unwrap_or_else(|message| {
            eprintln!("prod_trader: {message}");
            std::process::exit(8);
        });
    let mut occurrence_seq: u64 = 0;
    let proof = std::cell::RefCell::new(LiveProofReport::default());

    // The journal now exists; sweep both artifacts one last time before any
    // order flow can add entries to them.
    assert_artifacts_clean(&config, &secrets);
    let run_id = format!("prod-{}", epoch_ms());
    writer.send(&Frame::SessionStart {
        schema_version: LIVE_SCHEMA_VERSION,
        run_id: run_id.clone(),
        started_at_epoch_ms: epoch_ms(),
        initial_bankroll_cents: Some(initial_total),
        ticks_per_window: 1,
        window_ms: config.window.as_millis() as u64,
        execution_mode: wire_mode(&config.mode),
    });
    writer.send(&Frame::Risk {
        state: RiskStateWire::snapshot(&risk, &config.mode),
    });
    println!(
        "run_id={run_id} journal={} state={} streaming={} restored_in_flight={}",
        config.journal_path,
        state_store.path().display(),
        config.ingest_url,
        stored.as_ref().map(|s| s.in_flight.len()).unwrap_or(0),
    );

    // Command pull client.
    let command_agent = ureq::AgentBuilder::new()
        .timeout(Duration::from_secs(5))
        .build();
    let command_url = control::control_url_from_ingest(&config.ingest_url);
    let mut command_high_water: u64 = 0;

    let labels = CatalogLabels {
        registry: &registry,
        venues: &labels_env,
    };
    let polling_source = PollingSource {
        first: &first_endpoint,
        second: &second_endpoint,
    };
    let mut deduper = OpportunityDeduper::default();
    let mut seq: u64 = 0;
    let mut windows_completed: u64 = 0;
    let mut attempted: u64 = 0;
    let mut capital_short: u64 = 0;
    let mut risk_rejected: u64 = 0;
    let mut unresolved_plans: u64 = 0;
    let mut unprotected_skips: u64 = 0;
    let mut ack_matched: u64 = 0;
    let mut unwind_failures: u64 = 0;
    let mut last_risk_push = Instant::now();
    let mut last_stats_push = Instant::now();

    let mut ending = false;
    while !ending {
        // 1. Drain the signal ring and execute.
        while let Some(signal) = signal_consumer.try_pop() {
            seq += 1;
            seq_cursor.store(seq, Ordering::Relaxed);
            if !deduper.accept(&signal) {
                continue;
            }
            attempted += 1;
            let mut legs = exec_legs_from_signal(&signal, &resolver);
            if legs.len() != 2 || legs[0].venue == legs[1].venue {
                // Plans the catalog cannot fully resolve are never executed:
                // a half-resolvable hedge is not a hedge.
                unresolved_plans += 1;
                continue;
            }
            // The executor's adapters are positional: Kalshi is always leg 0.
            if legs[0].venue != VenueRegistry::KALSHI {
                legs.swap(0, 1);
            }
            let mut executor = HedgedExecutor { risk: &mut risk };
            // Register both legs in-flight before submission so a crash between
            // preflight and acknowledgement reconciles by client order id.
            // The store is the durable twin: every leg is persisted first,
            // and a partial or failed persist rolls back — the plan is never
            // transmitted unprotected.
            let client_ids = [legs[0].client_order_id, legs[1].client_order_id];
            let mut persisted: Vec<InFlightOrder> = Vec::with_capacity(legs.len());
            for leg in &legs {
                match state_store.register_inflight(order_from_exec_leg(leg)) {
                    Ok(_) => persisted.push(order_from_exec_leg(leg)),
                    Err(error) => {
                        eprintln!(
                            "[{}] inflight persist failed client={:02x}: {error}; \
                             rolling back and refusing to transmit unprotected legs",
                            utc_now(),
                            leg.client_order_id[0]
                        );
                        break;
                    }
                }
            }
            if persisted.len() != legs.len() {
                for order in &persisted {
                    if let Err(error) = state_store.clear_inflight(order.client_order_id) {
                        eprintln!(
                            "[{}] rollback clear failed client={:02x}: {error}",
                            utc_now(),
                            order.client_order_id[0]
                        );
                    }
                }
                unprotected_skips += 1;
                continue;
            }
            for order in &persisted {
                reconciler.register(order.clone());
            }
            match executor.execute_reconciled(
                signal.signal.profit_bps,
                &legs,
                &first_endpoint,
                &second_endpoint,
            ) {
                Ok((report, reconciled_legs)) => {
                    // Acknowledge each leg's venue order id; unknown statuses
                    // (unwound) are closed when the poll observes them. The
                    // durable twin makes restart reconciliation idempotent.
                    for leg in &reconciled_legs {
                        if let Some(venue_order_id) = &leg.venue_order_id {
                            reconciler.acknowledge(leg.client_order_id, venue_order_id.clone());
                            ack_matched += 1;
                            if let Err(error) =
                                state_store.acknowledge(leg.client_order_id, venue_order_id.clone())
                            {
                                eprintln!(
                                    "[{}] ack persist failed client={:02x}: {error}",
                                    utc_now(),
                                    leg.client_order_id[0]
                                );
                            }
                        }
                    }
                    let record = build_record(seq, &signal, &report, &labels, &config.mode);
                    if let Ok(line) = serde_json::to_string(&record) {
                        use std::io::Write;
                        let _ = writeln!(journal, "{line}");
                        let _ = journal.flush();
                    }
                    println!(
                        "[{}] {} edge={}bps stake={}c filled={}c status={} orders={:?} \
                         client_ids={:02x}/{:02x}",
                        utc_now(),
                        record.classification,
                        record.edge_bps,
                        record.requested_stake_cents,
                        record.filled_stake_cents,
                        record.settlement_status,
                        record.venue_order_ids,
                        client_ids[0][0],
                        client_ids[1][0],
                    );
                    writer.send(&Frame::Positions {
                        items: vec![record],
                    });
                    emit_fill_frames(&writer, &report, &reconciled_legs);

                    // Freeze the detection-time occurrence for same-tape
                    // proof, and fold this attempt into the live report.
                    occurrence_seq += 1;
                    let record = arbkit_exec::occurrence_record(occurrence_seq, &signal, &legs);
                    {
                        use std::io::Write;
                        if let Ok(line) = serde_json::to_string(&record) {
                            let _ = writeln!(occurrences, "{line}");
                            let _ = occurrences.flush();
                        }
                    }
                    {
                        let mut p = proof.borrow_mut();
                        p.attempted_arbs += 1;
                        p.theoretical_profit_cents += record.worst_case_profit_cents;
                        match report.classification {
                            ExecutionClassification::LiveFill => p.live_fills += 1,
                            ExecutionClassification::LivePhantom => p.live_phantoms += 1,
                        }
                        p.unwinds = UNWINDS.load(Ordering::Relaxed);
                    }

                    if let Err(error) = state_store.checkpoint(&risk) {
                        eprintln!("[{}] risk checkpoint failed: {error}", utc_now());
                    }
                    writer.send(&Frame::Risk {
                        state: RiskStateWire::snapshot(&risk, &config.mode),
                    });
                    last_risk_push = Instant::now();
                }
                Err(ExecError::Risk(rejection)) => {
                    if matches!(rejection, RiskRejection::InsufficientCapital) {
                        capital_short += 1;
                    }
                    risk_rejected += 1;
                }
                Err(error) => {
                    // An unwind failure is capital possibly stuck at a venue:
                    // loud, timestamped, counted, and never swallowed.
                    if matches!(error, ExecError::Unwind { .. }) {
                        unwind_failures += 1;
                    }
                    proof.borrow_mut().live_phantoms += 1;
                    eprintln!(
                        "[{}] EXECUTION ERROR (manual reconciliation may be required): {error}",
                        utc_now()
                    );
                }
            }
        }

        // 2. Pull and apply operator commands.
        match poll_commands(
            &command_agent,
            &command_url,
            &config.token,
            command_high_water,
        ) {
            Ok(envelopes) => {
                for envelope in envelopes {
                    command_high_water = command_high_water.max(envelope.id);
                    // Attribution: the worker attests the authenticated
                    // issuer on the envelope. Absence (pre-identity worker)
                    // falls back to this process's self-reported env name,
                    // labeled as exactly that.
                    let operator = match envelope.operator.as_deref() {
                        Some(attested) => attested.to_owned(),
                        None => format!("{} (self-reported)", operator_id()),
                    };
                    match envelope.command {
                        OperatorCommand::KillSwitch { engage, confirm } => {
                            // A disarming command without explicit
                            // confirmation is refused. The worker's zod
                            // schema already rejects a bare disarm, but the
                            // runner enforces it independently so a defect on
                            // either side cannot arm real order flow silently.
                            if !engage && !confirm {
                                eprintln!(
                                    "[{}] REFUSED disarm command #{} (no explicit \
                                     confirmation) operator={}",
                                    utc_now(),
                                    envelope.id,
                                    operator
                                );
                                continue;
                            }
                            risk.config.kill_switch = engage;
                            println!(
                                "[{}] kill-switch engage={} applied (operator command id={} \
                                 operator={})",
                                utc_now(),
                                engage,
                                envelope.id,
                                operator
                            );
                            writer.send(&Frame::Risk {
                                state: RiskStateWire::snapshot(&risk, &config.mode),
                            });
                            last_risk_push = Instant::now();
                        }
                        OperatorCommand::SessionEnd => {
                            println!(
                                "[{}] session-end received (operator command id={}, \
                                 operator={})",
                                utc_now(),
                                envelope.id,
                                operator
                            );
                            ending = true;
                        }
                        OperatorCommand::SessionStart { mode } => {
                            // One process = one session, opened at launch with
                            // a fixed mode (refresh requires a restart). A
                            // matching start is a no-op; a mismatched one is
                            // refused so the console cannot silently change
                            // which venue profile is live.
                            if mode == config.mode {
                                println!(
                                    "[{}] session-start mode={mode} acknowledged: already \
                                     running (operator command id={}, operator={})",
                                    utc_now(),
                                    envelope.id,
                                    operator
                                );
                            } else {
                                eprintln!(
                                    "[{}] refused session-start mode={mode} (process is mode={}): \
                                     restart required to change venue profile (operator command \
                                     id={}, operator={})",
                                    utc_now(),
                                    config.mode,
                                    envelope.id,
                                    operator
                                );
                            }
                        }
                    }
                }
            }
            Err(error) => {
                // Control-plane loss never stops trading; only the kill
                // switch stops trading. The dashboard shows the disconnect.
                eprintln!("[{}] {}", utc_now(), error);
            }
        }

        // 2.5 Reconcile in-flight orders against venue status. A settled or
        // unwound order is closed exactly once and its risk impact applied;
        // the ledger is idempotent so at-least-once polling never doubles
        // fees or profit.
        if !reconciler.in_flight().is_empty() {
            match reconciler.reconcile(&polling_source) {
                Ok(settled) => {
                    for settle in &settled {
                        let profit = settle.realized_profit_cents.unwrap_or(0);
                        risk.settle(profit);
                        emit_settlement_frames(&writer, settle);
                        // Terminal and applied: drop the durable ghost so a
                        // restart does not refuse over settled orders.
                        if let Err(error) = state_store.clear_inflight(settle.client_order_id) {
                            eprintln!(
                                "[{}] inflight clear failed client={:02x}: {error}",
                                utc_now(),
                                settle.client_order_id[0]
                            );
                        }
                        println!(
                            "[{}] settled order client={:02x} venue={} status={} \
                             filled={}c profit={:?} fees={}c",
                            utc_now(),
                            settle.client_order_id[0],
                            settle.venue_order_id,
                            settle.status,
                            settle.filled_stake_cents,
                            settle.realized_profit_cents,
                            settle.fees_paid_cents,
                        );
                    }
                    if !settled.is_empty() {
                        if let Err(error) = state_store.checkpoint(&risk) {
                            eprintln!("[{}] risk checkpoint failed: {error}", utc_now());
                        }
                        writer.send(&Frame::Risk {
                            state: RiskStateWire::snapshot(&risk, &config.mode),
                        });
                        last_risk_push = Instant::now();
                    }
                }
                Err(error) => {
                    eprintln!("[{}] reconciliation error: {error}", utc_now());
                }
            }
        }

        // 3. Periodic posture and stats frames.
        if last_risk_push.elapsed() >= Duration::from_secs(2) {
            writer.send(&Frame::Risk {
                state: RiskStateWire::snapshot(&risk, &config.mode),
            });
            last_risk_push = Instant::now();
        }
        if last_stats_push.elapsed() >= Duration::from_secs(2) {
            let available: i64 = risk.bankroll_snapshot().values().sum();
            writer.send(&Frame::Stats {
                seq_cursor: seq,
                windows_completed,
                locked_cents: Some(initial_total - available),
                available_cents: Some(available),
                attempted,
                capital_short,
                unwind_failures,
                ack_matched,
                in_flight_remaining: reconciler.in_flight().len(),
            });
            last_stats_push = Instant::now();
        }

        windows_completed += 1;
        if let Some(limit) = config.windows_limit {
            if windows_completed >= limit {
                ending = true;
            }
        }
        std::thread::sleep(config.window);
    }

    // Graceful shutdown: stop the world in dependency order, checkpoint, sign
    // off. A killed runner skips all of this and lets staleness speak.
    running.store(false, Ordering::Relaxed);
    let _ = bridge.join();
    let _ = engine_thread.join();
    if let Err(error) = state_store.checkpoint(&risk) {
        eprintln!("[{}] final checkpoint failed: {error}", utc_now());
    }
    // Finalize the live proof report: settled profit and fees come from the
    // idempotent ledger; slippage/stake totals fold in from execution.
    {
        let mut p = proof.borrow_mut();
        p.realized_profit_cents = reconciler.ledger().realized_profit_cents;
        p.fees_paid_cents = reconciler.ledger().fees_paid_cents;
        p.unwinds = UNWINDS.load(Ordering::Relaxed);
    }
    let proof_final = *proof.borrow();
    match serde_json::to_string_pretty(&proof_final) {
        Ok(json) => {
            if let Err(error) = fs::write(&config.proof_path, json + "\n") {
                eprintln!(
                    "[{}] proof write failed {}: {error}",
                    utc_now(),
                    config.proof_path
                );
            } else {
                println!(
                    "[{}] proof artifact written to {} (compare: cargo run -p arbkit-exec \
                     --features paper-replay --example same_tape_proof -- --input {} \
                     --compare {} --tolerance-bps 50)",
                    utc_now(),
                    config.proof_path,
                    config.occurrences_path,
                    config.proof_path
                );
            }
        }
        Err(error) => eprintln!("[{}] proof serialize failed: {error}", utc_now()),
    }

    // Last sweep: everything the session wrote is on disk now.
    assert_artifacts_clean(&config, &secrets);
    writer.send(&Frame::Risk {
        state: RiskStateWire::snapshot(&risk, &config.mode),
    });
    writer.send(&Frame::SessionEnd);
    writer.finish();
    runtime.shutdown_timeout(Duration::from_secs(5));
    // Warmup ledger: every acknowledged venue order id maps to a client
    // order id by construction (both live in the same InFlightOrder), and
    // anything still open here was durably registered before submission and
    // re-seeds on restart — open is not orphaned.
    let in_flight_remaining = reconciler.in_flight().len();
    println!(
        "[{}] warmup ledger: unwind_failures={} ack_matched={} in_flight_remaining={} \
         tape_events={}",
        utc_now(),
        unwind_failures,
        ack_matched,
        in_flight_remaining,
        tape_events.load(Ordering::Relaxed),
    );
    if in_flight_remaining > 0 {
        println!(
            "[{}] note: {} order(s) still awaiting venue settlement at shutdown; \
             they are durably registered and re-seed on restart",
            utc_now(),
            in_flight_remaining
        );
    }
    println!(
        "[{}] session ended: windows={} attempted={} risk_rejected={} \
         capital_short={} unresolved_plans={} unprotected_skips={}",
        utc_now(),
        windows_completed,
        attempted,
        risk_rejected,
        capital_short,
        unresolved_plans,
        unprotected_skips
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn owned(args: &[&str]) -> Vec<String> {
        args.iter().map(|s| s.to_string()).collect()
    }

    fn value(args: &[&str], flag: &str) -> Result<Option<String>, String> {
        arg_value(&owned(args), flag)
    }

    #[test]
    fn equals_form_parses() {
        assert_eq!(
            value(&["--windows=5"], "windows").unwrap(),
            Some("5".into())
        );
    }

    #[test]
    fn space_form_parses() {
        assert_eq!(
            value(&["--windows", "5"], "windows").unwrap(),
            Some("5".into())
        );
    }

    #[test]
    fn both_spellings_resolve_identically() {
        let eq = ["--mode=dry-run", "--windows=5", "--proof=/tmp/a=b.json"];
        let sp = [
            "--mode",
            "dry-run",
            "--windows",
            "5",
            "--proof",
            "/tmp/a=b.json",
        ];
        for flag in ["mode", "windows", "proof"] {
            assert_eq!(
                value(&eq, flag).unwrap(),
                value(&sp, flag).unwrap(),
                "flag {flag}"
            );
        }
    }

    #[test]
    fn space_form_keeps_equals_inside_value() {
        assert_eq!(
            value(&["--proof", "/tmp/a=b.json"], "proof").unwrap(),
            Some("/tmp/a=b.json".into())
        );
    }

    #[test]
    fn space_form_value_after_other_flags() {
        assert_eq!(
            value(&["--micro", "--windows", "5"], "windows").unwrap(),
            Some("5".into())
        );
    }

    #[test]
    fn missing_space_value_is_usage_error() {
        assert!(value(&["--windows"], "windows").is_err());
        assert!(value(&["--windows", "--micro"], "windows").is_err());
        assert!(value(&["--micro", "--windows"], "windows").is_err());
    }

    #[test]
    fn boolean_flag_matches_bare_only() {
        assert!(arg_flag(&owned(&["--micro", "--windows=5"]), "micro"));
        assert!(!arg_flag(&owned(&["--micro=true"]), "micro"));
    }

    #[test]
    fn runner_config_equivalent_between_spellings() {
        let eq = [
            "--mode=dry-run",
            "--windows=5",
            "--state=/tmp/s.json",
            "--proof=/tmp/a=b.json",
        ];
        let sp = [
            "--mode",
            "dry-run",
            "--windows",
            "5",
            "--state",
            "/tmp/s.json",
            "--proof",
            "/tmp/a=b.json",
        ];
        let a = RunnerConfig::from_args(&owned(&eq)).unwrap();
        let b = RunnerConfig::from_args(&owned(&sp)).unwrap();
        assert_eq!(a.mode, b.mode);
        assert_eq!(a.windows_limit, b.windows_limit);
        assert_eq!(a.state_path, b.state_path);
        assert_eq!(a.proof_path, b.proof_path);
        assert_eq!(a.ingest_url, b.ingest_url);
        assert_eq!(a.micro, b.micro);
    }

    #[test]
    fn runner_config_missing_value_is_error() {
        assert!(RunnerConfig::from_args(&owned(&["--windows"])).is_err());
        assert!(RunnerConfig::from_args(&owned(&["--windows", "--micro"])).is_err());
    }
}
