//! Order execution boundary.
//!
//! This crate deliberately does not appear in the detector's dependency
//! graph. `HedgedExecutor` runs after a `SignalEvent` leaves the SPSC ring;
//! allocations, network I/O, and risk bookkeeping are therefore confined to
//! this async/application boundary. The default adapter is dry-run and never
//! sends an HTTP request.

#![forbid(unsafe_code)]
#![deny(missing_docs)]

use std::collections::HashMap;

use arbkit_core::{Cents, Prob, VenueId};
use arbkit_match::{VenueInstrument, VenueInstrumentMap};
use serde::{Deserialize, Serialize};
use thiserror::Error;

pub mod proof;
#[cfg(feature = "paper-replay")]
pub use proof::occurrence_record;
pub use proof::{compare_tape, LiveProofReport, TapeComparison};
#[cfg(feature = "paper-replay")]
pub use proof::{replay_paper_tape, OccurrenceLeg, OccurrenceRecord};

#[cfg(feature = "live")]
pub mod kalshi;
#[cfg(feature = "live")]
pub use kalshi::{KalshiConfig, KalshiError, KalshiExecutionAdapter, OrderStatus};
#[cfg(feature = "live")]
pub mod polymarket;
#[cfg(feature = "live")]
pub use polymarket::{
    PolymarketConfig, PolymarketError, PolymarketExecutionAdapter, PolymarketOrderStatus,
    TimeInForce,
};

pub mod state;
pub use state::{
    DurableRiskState, FeedCircuitBreaker, FillEvent, InFlightOrder, PersistedExecLeg, RateLimiter,
    ReconciliationLedger, RiskStateStore,
};

pub mod reconcile;
pub use reconcile::{is_terminal_status, Reconciler, Settlement, SettlementSource};

pub mod secrets;
pub use secrets::SecretScan;

/// Cancel every known accepted order during an emergency flatten operation.
pub fn emergency_flatten<A: VenueAdapter>(
    adapter: &A,
    orders: &[OrderResult],
) -> Result<usize, ExecError> {
    let mut flattened = 0;
    for order in orders {
        adapter
            .unwind(order)
            .map_err(|message| ExecError::Unwind { venue: 0, message })?;
        flattened += 1;
    }
    Ok(flattened)
}

/// Resolves a signal plan's venue/outcome pair into an execution instrument.
pub trait InstrumentResolver {
    /// Return the venue-specific instrument for one planned leg.
    fn resolve(
        &self,
        venue: VenueId,
        outcome: arbkit_core::OutcomeId,
    ) -> Option<VenueInstrumentRef>;
}

/// Convert an engine signal into executable legs after it leaves the hot loop.
pub fn exec_legs_from_signal(
    signal: &arbkit_engine::SignalEvent,
    resolver: &impl InstrumentResolver,
) -> Vec<ExecLeg> {
    signal.plan[..signal.plan_len as usize]
        .iter()
        .filter_map(|leg| {
            resolver
                .resolve(leg.venue, leg.outcome)
                .map(|instrument| ExecLeg {
                    venue: leg.venue,
                    instrument,
                    limit_price: leg.quoted,
                    stake_cents: leg.capacity,
                    client_order_id: client_order_id(signal.market_id, leg.venue, leg.outcome),
                })
        })
        .collect()
}

fn client_order_id(market: u32, venue: VenueId, outcome: arbkit_core::OutcomeId) -> [u8; 16] {
    let mut id = [0; 16];
    id[..4].copy_from_slice(&market.to_le_bytes());
    id[4..6].copy_from_slice(&venue.to_le_bytes());
    id[6..10].copy_from_slice(&outcome.to_le_bytes());
    id
}

/// Post-engine opportunity deduplicator; safe to allocate outside the hot loop.
#[derive(Debug, Default)]
pub struct OpportunityDeduper {
    seen: std::collections::HashSet<[u8; 16]>,
}

impl OpportunityDeduper {
    /// Return true only for the first occurrence of an unchanged signal plan.
    pub fn accept(&mut self, signal: &arbkit_engine::SignalEvent) -> bool {
        let mut key = client_order_id(
            signal.market_id,
            signal.plan_len as u16,
            signal.signal.profit_bps,
        );
        for leg in signal.plan.iter().take(signal.plan_len as usize) {
            key[0] ^= leg.venue as u8;
            key[1] ^= leg.outcome as u8;
            key[2] ^= leg.quoted.ppm() as u8;
        }
        self.seen.insert(key)
    }
    /// Clear deduplication state at a catalog/session boundary.
    pub fn clear(&mut self) {
        self.seen.clear();
    }
}

/// An instrument identifier suitable for an order request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum VenueInstrumentRef {
    /// Kalshi market ticker.
    Kalshi(String),
    /// Polymarket token id in its fixed-width form.
    Polymarket([u8; 32]),
}

/// One order leg in a hedge.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecLeg {
    /// Venue receiving the order.
    pub venue: VenueId,
    /// Venue-specific instrument.
    pub instrument: VenueInstrumentRef,
    /// Buy-side limit price.
    pub limit_price: Prob,
    /// Stake requested in cents.
    pub stake_cents: Cents,
    /// Idempotency key.
    pub client_order_id: [u8; 16],
}

/// Whether order submission is simulated or enabled by the caller.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExecMode {
    /// Log intended orders only.
    DryRun,
    /// Permit a live adapter to submit.
    Live,
}

/// Conservative limits applied before any order is submitted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct RiskConfig {
    /// Maximum stake for a single leg.
    pub max_stake_per_leg_cents: Cents,
    /// Maximum permitted loss in the current session/day.
    pub max_daily_loss_cents: Cents,
    /// Maximum number of open trades.
    pub max_open_trades: u32,
    /// Minimum detected edge in basis points.
    pub min_edge_bps: u32,
    /// Hard stop, normally set from `ARBKIT_KILL_SWITCH=1`.
    pub kill_switch: bool,
}

impl Default for RiskConfig {
    fn default() -> Self {
        Self {
            max_stake_per_leg_cents: 5_000,
            max_daily_loss_cents: 50_000,
            max_open_trades: 1,
            min_edge_bps: 50,
            kill_switch: true,
        }
    }
}

/// Per-leg stake cap for micro-live: two contracts at any legal binary price
/// (≤ 99¢) can never exceed this, so the cap *is* the two-contract limit
/// regardless of what the book quotes.
pub const MICRO_MAX_STAKE_PER_LEG_CENTS: Cents = 200;

/// Micro-live policy: the daily loss budget is one worst-case leg loss — the
/// entire stake of a single leg — never more.
pub fn micro_live_config(env: RiskConfig) -> RiskConfig {
    RiskConfig {
        max_stake_per_leg_cents: env
            .max_stake_per_leg_cents
            .min(MICRO_MAX_STAKE_PER_LEG_CENTS),
        max_daily_loss_cents: env.max_daily_loss_cents.min(MICRO_MAX_STAKE_PER_LEG_CENTS),
        ..env
    }
}

/// A validated risk decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RiskRejection {
    /// Kill switch is active.
    KillSwitch,
    /// Signal edge is below the configured floor.
    EdgeTooSmall,
    /// Requested leg exceeds its cap.
    StakeCap,
    /// Daily loss cap is exhausted.
    DailyLoss,
    /// Too many trades are open.
    OpenTrades,
    /// The venue bankroll cannot reserve the leg.
    InsufficientCapital,
}

/// Execution-layer errors.
#[derive(Debug, Error)]
pub enum ExecError {
    /// Risk gate rejected the attempt.
    #[error("risk rejected execution: {0:?}")]
    Risk(RiskRejection),
    /// An adapter rejected or failed an order.
    #[error("venue {venue} order failed: {message}")]
    Venue {
        /// Venue identifier.
        venue: VenueId,
        /// Adapter-provided failure message.
        message: String,
    },
    /// A hedge leg could not be unwound.
    #[error("unwind failed for venue {venue}: {message}")]
    Unwind {
        /// Venue identifier.
        venue: VenueId,
        /// Adapter-provided failure message.
        message: String,
    },
}

/// Result returned by a venue adapter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrderResult {
    /// Venue order id, if accepted.
    pub order_id: String,
    /// Requested stake filled in cents.
    pub filled_stake_cents: Cents,
}

/// Adapter boundary for a venue's FOK/IOC API.
pub trait VenueAdapter {
    /// Submit one leg. Implementations own authentication and HTTP details.
    fn submit(&self, leg: &ExecLeg) -> Result<OrderResult, String>;
    /// Cancel or flatten a previously filled leg.
    fn unwind(&self, order: &OrderResult) -> Result<(), String>;
}

/// Adapter that records intended orders and never performs I/O.
#[derive(Debug, Default)]
pub struct DryRunAdapter;
impl VenueAdapter for DryRunAdapter {
    fn submit(&self, leg: &ExecLeg) -> Result<OrderResult, String> {
        Ok(OrderResult {
            order_id: format!("dry-run-{}-{}", leg.venue, hex_id(&leg.client_order_id)),
            filled_stake_cents: leg.stake_cents,
        })
    }
    fn unwind(&self, _order: &OrderResult) -> Result<(), String> {
        Ok(())
    }
}

/// Account and risk state owned by the execution task.
#[derive(Debug, Clone)]
pub struct RiskGate {
    /// Immutable policy.
    pub config: RiskConfig,
    /// Current realized loss, represented as a positive number.
    pub daily_loss_cents: Cents,
    /// Number of unsettled trades.
    pub open_trades: u32,
    bankroll: HashMap<VenueId, Cents>,
}

impl RiskGate {
    /// Create a gate with per-venue available balances.
    pub fn new(config: RiskConfig, balances: impl IntoIterator<Item = (VenueId, Cents)>) -> Self {
        Self {
            config,
            daily_loss_cents: 0,
            open_trades: 0,
            bankroll: balances.into_iter().collect(),
        }
    }

    /// Restore a gate from durable fields without releasing reservations.
    pub fn from_durable(
        config: RiskConfig,
        daily_loss_cents: Cents,
        open_trades: u32,
        balances: impl IntoIterator<Item = (VenueId, Cents)>,
    ) -> Self {
        Self {
            config,
            daily_loss_cents,
            open_trades,
            bankroll: balances.into_iter().collect(),
        }
    }

    /// Validate and reserve all legs before submission.
    pub fn preflight(&mut self, edge_bps: u32, legs: &[ExecLeg]) -> Result<(), RiskRejection> {
        if self.config.kill_switch {
            return Err(RiskRejection::KillSwitch);
        }
        if edge_bps < self.config.min_edge_bps {
            return Err(RiskRejection::EdgeTooSmall);
        }
        if self.daily_loss_cents >= self.config.max_daily_loss_cents {
            return Err(RiskRejection::DailyLoss);
        }
        if self.open_trades >= self.config.max_open_trades {
            return Err(RiskRejection::OpenTrades);
        }
        let mut reserved = Vec::new();
        for leg in legs {
            if leg.stake_cents <= 0 || leg.stake_cents > self.config.max_stake_per_leg_cents {
                self.release(&reserved);
                return Err(RiskRejection::StakeCap);
            }
            let balance = self.bankroll.entry(leg.venue).or_default();
            if *balance < leg.stake_cents {
                self.release(&reserved);
                return Err(RiskRejection::InsufficientCapital);
            }
            *balance -= leg.stake_cents;
            reserved.push((leg.venue, leg.stake_cents));
        }
        self.open_trades += 1;
        Ok(())
    }

    fn release(&mut self, reserved: &[(VenueId, Cents)]) {
        for &(venue, cents) in reserved {
            *self.bankroll.entry(venue).or_default() += cents;
        }
    }
    /// Return reserved capital for a failed hedge.
    pub fn release_trade(&mut self, legs: &[ExecLeg]) {
        for leg in legs {
            *self.bankroll.entry(leg.venue).or_default() += leg.stake_cents;
        }
        self.open_trades = self.open_trades.saturating_sub(1);
    }
    /// Record settlement and close one trade.
    pub fn settle(&mut self, realized_profit_cents: Cents) {
        if realized_profit_cents < 0 {
            self.daily_loss_cents = self.daily_loss_cents.saturating_add(-realized_profit_cents);
        }
        self.open_trades = self.open_trades.saturating_sub(1);
    }

    /// Snapshot per-venue available capital for durable state.
    pub fn bankroll_snapshot(&self) -> HashMap<VenueId, Cents> {
        self.bankroll.clone()
    }
}

/// Authoritative result classification sent to the ledger/dashboard.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ExecutionClassification {
    /// Every leg filled.
    LiveFill,
    /// One or more legs failed and filled legs were unwound.
    LivePhantom,
}

/// Reconciled result for a hedge attempt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecutionReport {
    /// Classification for proof tooling.
    pub classification: ExecutionClassification,
    /// Venue order ids that were accepted.
    pub venue_order_ids: Vec<String>,
    /// Requested amount across legs.
    pub requested_stake_cents: Cents,
    /// Filled amount across legs.
    pub filled_stake_cents: Cents,
    /// Realized profit, or null until settlement.
    pub realized_profit_cents: Option<Cents>,
    /// Open, settled, or unwound.
    pub settlement_status: String,
    /// Failure reason for a phantom attempt.
    pub reason: Option<String>,
}

/// Execute an all-or-nothing two-leg hedge.
pub struct HedgedExecutor<'a> {
    /// Risk gate.
    pub risk: &'a mut RiskGate,
}

impl<'a> HedgedExecutor<'a> {
    /// Preflight, submit both legs, and unwind any partial hedge.
    ///
    /// The two legs are submitted **concurrently** so neither is priced
    /// against a book that moved during the other's round-trip — a sequential
    /// pair of blocking venue calls is how a clean fill turns into a phantom.
    /// Concurrency uses scoped threads on borrowed adapters and blocks until
    /// both results are in, so the trait seam stays synchronous and testable
    /// under `cargo test` with no async runtime. The runner calls this from
    /// its own execution task, off the engine hot loop.
    pub fn execute<A: VenueAdapter + Sync, B: VenueAdapter + Sync>(
        &mut self,
        edge_bps: u32,
        legs: &[ExecLeg],
        first: &A,
        second: &B,
    ) -> Result<ExecutionReport, ExecError> {
        self.execute_reconciled(edge_bps, legs, first, second)
            .map(|(report, _)| report)
    }

    /// Same as [`HedgedExecutor::execute`], but also returns the per-leg
    /// reconciliation view (`client_order_id` -> venue order id -> status) so
    /// a runner can register/acknowledge orders for settlement polling. The
    /// wire-facing report is unchanged.
    pub fn execute_reconciled<A: VenueAdapter + Sync, B: VenueAdapter + Sync>(
        &mut self,
        edge_bps: u32,
        legs: &[ExecLeg],
        first: &A,
        second: &B,
    ) -> Result<(ExecutionReport, Vec<ReconciledLeg>), ExecError> {
        if legs.len() != 2 {
            return Err(ExecError::Venue {
                venue: 0,
                message: "exactly two hedge legs are required".into(),
            });
        }
        self.risk
            .preflight(edge_bps, legs)
            .map_err(ExecError::Risk)?;
        let (first_result, second_result) = std::thread::scope(|scope| {
            let first_join = scope.spawn(|| first.submit(&legs[0]));
            let second_join = scope.spawn(|| second.submit(&legs[1]));
            (
                join_submit(first_join, |m| format!("first leg submit panicked: {m}")),
                join_submit(second_join, |m| format!("second leg submit panicked: {m}")),
            )
        });
        let accepted_first = first_result.as_ref().ok().map(|r| r.order_id.clone());
        let accepted_second = second_result.as_ref().ok().map(|r| r.order_id.clone());
        let mut accepted = Vec::new();
        if let Some(ref id) = accepted_first {
            accepted.push((0usize, id.clone()));
        }
        if let Some(ref id) = accepted_second {
            accepted.push((1usize, id.clone()));
        }
        let success = first_result
            .as_ref()
            .map(|r| r.filled_stake_cents == legs[0].stake_cents)
            .unwrap_or(false)
            && second_result
                .as_ref()
                .map(|r| r.filled_stake_cents == legs[1].stake_cents)
                .unwrap_or(false);
        let reconciled = reconciled_legs(legs, accepted_first, accepted_second, success);
        if success {
            let report = ExecutionReport {
                classification: ExecutionClassification::LiveFill,
                venue_order_ids: accepted.into_iter().map(|(_, id)| id).collect(),
                requested_stake_cents: legs.iter().map(|l| l.stake_cents).sum(),
                filled_stake_cents: legs.iter().map(|l| l.stake_cents).sum(),
                realized_profit_cents: None,
                settlement_status: "open".into(),
                reason: None,
            };
            return Ok((report, reconciled));
        }
        if let Ok(result) = first_result {
            first.unwind(&result).map_err(|message| ExecError::Unwind {
                venue: legs[0].venue,
                message,
            })?;
        }
        if let Ok(result) = second_result {
            second
                .unwind(&result)
                .map_err(|message| ExecError::Unwind {
                    venue: legs[1].venue,
                    message,
                })?;
        }
        self.risk.release_trade(legs);
        let report = ExecutionReport {
            classification: ExecutionClassification::LivePhantom,
            venue_order_ids: accepted.into_iter().map(|(_, id)| id).collect(),
            requested_stake_cents: legs.iter().map(|l| l.stake_cents).sum(),
            filled_stake_cents: 0,
            realized_profit_cents: None,
            settlement_status: "unwound".into(),
            reason: Some("one or more legs rejected or partially filled".into()),
        };
        Ok((report, reconciled))
    }
}

/// Per-leg reconciliation view returned by
/// [`HedgedExecutor::execute_reconciled`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReconciledLeg {
    /// Idempotency key for this leg.
    pub client_order_id: [u8; 16],
    /// Venue receiving the order.
    pub venue: VenueId,
    /// Venue order id once accepted.
    pub venue_order_id: Option<String>,
    /// Requested stake.
    pub requested_stake_cents: Cents,
    /// Filled stake (0 if never accepted).
    pub filled_stake_cents: Cents,
    /// `open` (filled, waiting settlement) or `unwound` (rejected/flattened).
    pub status: String,
}

/// Build the per-leg reconciliation view, keying by client id so a runner can
/// match venue invoices without guessing at positional order.
fn reconciled_legs(
    legs: &[ExecLeg],
    accepted_first: Option<String>,
    accepted_second: Option<String>,
    filled: bool,
) -> Vec<ReconciledLeg> {
    vec![
        ReconciledLeg {
            client_order_id: legs[0].client_order_id,
            venue: legs[0].venue,
            venue_order_id: accepted_first,
            requested_stake_cents: legs[0].stake_cents,
            filled_stake_cents: if filled { legs[0].stake_cents } else { 0 },
            status: if filled { "open" } else { "unwound" }.into(),
        },
        ReconciledLeg {
            client_order_id: legs[1].client_order_id,
            venue: legs[1].venue,
            venue_order_id: accepted_second,
            requested_stake_cents: legs[1].stake_cents,
            filled_stake_cents: if filled { legs[1].stake_cents } else { 0 },
            status: if filled { "open" } else { "unwound" }.into(),
        },
    ]
}

/// Join a scoped adapter thread, collapsing a panic into a venue error
/// string. A panicking adapter must degrade to a reject/handle leg — never a
/// process abort on the execution boundary.
fn join_submit<T>(
    join: std::thread::ScopedJoinHandle<'_, Result<T, String>>,
    on_panic: impl FnOnce(String) -> String,
) -> Result<T, String> {
    join.join()
        .map_err(|payload| on_panic(panic_message(payload)))
        .and_then(|inner| inner)
}

/// Extract a message from a panic payload (or a generic one) for reporting.
fn panic_message(payload: Box<dyn std::any::Any + Send>) -> String {
    if let Some(message) = payload.downcast_ref::<&str>() {
        (*message).to_string()
    } else if let Some(message) = payload.downcast_ref::<String>() {
        message.clone()
    } else {
        "unknown panic".to_string()
    }
}

/// Convert a match catalog instrument into an execution reference.
pub fn instrument_ref(instrument: &VenueInstrument) -> Option<VenueInstrumentRef> {
    instrument
        .kalshi_ticker
        .clone()
        .map(VenueInstrumentRef::Kalshi)
        .or_else(|| instrument.poly_token_id.map(VenueInstrumentRef::Polymarket))
}

/// Return the active pair for an instrument map, preserving the catalog gate.
pub fn active_pair(
    map: &VenueInstrumentMap,
    market_id: u32,
) -> Option<&arbkit_match::VenueInstrumentPair> {
    map.get(market_id)
}

fn hex_id(id: &[u8; 16]) -> String {
    id.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn micro_live_caps_stake_and_daily_budget() {
        let env = RiskConfig {
            max_stake_per_leg_cents: 5_000,
            max_daily_loss_cents: 50_000,
            max_open_trades: 1,
            min_edge_bps: 50,
            kill_switch: true,
        };
        let micro = micro_live_config(env);
        // Two contracts at any price cannot exceed 200c; the daily budget is
        // one worst-case leg loss (the whole stake of one leg), never more.
        assert_eq!(micro.max_stake_per_leg_cents, MICRO_MAX_STAKE_PER_LEG_CENTS);
        assert_eq!(micro.max_daily_loss_cents, MICRO_MAX_STAKE_PER_LEG_CENTS);
        assert_eq!(micro.min_edge_bps, 50);

        // An operator who already set tighter caps is never loosened.
        let tighter = micro_live_config(RiskConfig {
            max_stake_per_leg_cents: 100,
            max_daily_loss_cents: 100,
            ..env
        });
        assert_eq!(tighter.max_stake_per_leg_cents, 100);
        assert_eq!(tighter.max_daily_loss_cents, 100);
    }
    struct Mock {
        fill: Cents,
        fail_unwind: bool,
    }
    impl VenueAdapter for Mock {
        fn submit(&self, leg: &ExecLeg) -> Result<OrderResult, String> {
            Ok(OrderResult {
                order_id: format!("{}", leg.venue),
                filled_stake_cents: self.fill,
            })
        }
        fn unwind(&self, _: &OrderResult) -> Result<(), String> {
            if self.fail_unwind {
                Err("unwind failed".into())
            } else {
                Ok(())
            }
        }
    }
    fn leg(venue: VenueId) -> ExecLeg {
        ExecLeg {
            venue,
            instrument: VenueInstrumentRef::Kalshi("x".into()),
            limit_price: Prob::from_cents(50).unwrap(),
            stake_cents: 100,
            client_order_id: [venue as u8; 16],
        }
    }
    #[test]
    fn kill_switch_blocks_orders() {
        let mut risk = RiskGate::new(RiskConfig::default(), [(1, 100), (2, 100)]);
        let mut ex = HedgedExecutor { risk: &mut risk };
        assert!(matches!(
            ex.execute(100, &[leg(1), leg(2)], &DryRunAdapter, &DryRunAdapter),
            Err(ExecError::Risk(RiskRejection::KillSwitch))
        ));
    }
    #[test]
    fn partial_fill_unwinds_and_releases_capital() {
        let cfg = RiskConfig {
            kill_switch: false,
            ..RiskConfig::default()
        };
        let mut risk = RiskGate::new(cfg, [(1, 100), (2, 100)]);
        let mut ex = HedgedExecutor { risk: &mut risk };
        let result = ex
            .execute(
                100,
                &[leg(1), leg(2)],
                &Mock {
                    fill: 50,
                    fail_unwind: false,
                },
                &Mock {
                    fill: 100,
                    fail_unwind: false,
                },
            )
            .unwrap();
        assert_eq!(result.classification, ExecutionClassification::LivePhantom);
        assert_eq!(risk.open_trades, 0);
    }
    #[test]
    fn submits_both_legs_concurrently() {
        /// An adapter whose `submit` blocks on a 2-party barrier so it cannot
        /// return until *both* legs have entered the critical section. Under
        /// the concurrent executor both scoped threads reach the barrier and
        /// the hedge fills clean; under a sequential executor the first leg
        /// would wait on the barrier forever (the test hangs rather than
        /// silently passing a wrong result). That asymmetry is the point.
        struct BothLock {
            barrier: std::sync::Arc<std::sync::Barrier>,
        }
        impl VenueAdapter for BothLock {
            fn submit(&self, _leg: &ExecLeg) -> Result<OrderResult, String> {
                self.barrier.wait();
                Ok(OrderResult {
                    order_id: "concurrent".into(),
                    filled_stake_cents: _leg.stake_cents,
                })
            }
            fn unwind(&self, _: &OrderResult) -> Result<(), String> {
                Ok(())
            }
        }

        let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
        let first = BothLock {
            barrier: std::sync::Arc::clone(&barrier),
        };
        let second = BothLock { barrier };

        let cfg = RiskConfig {
            kill_switch: false,
            ..RiskConfig::default()
        };
        let mut risk = RiskGate::new(cfg, [(1, 1000), (2, 1000)]);
        let mut ex = HedgedExecutor { risk: &mut risk };
        let result = ex.execute(100, &[leg(1), leg(2)], &first, &second).unwrap();
        assert_eq!(result.classification, ExecutionClassification::LiveFill);
        assert_eq!(result.filled_stake_cents, 200);
    }
}
