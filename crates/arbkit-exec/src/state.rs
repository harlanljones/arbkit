//! Durable execution state and venue-feed safety controls.

use crate::{ExecLeg, RiskConfig, RiskGate};
use arbkit_core::{Cents, VenueId};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// Serializable snapshot required to resume risk decisions safely.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DurableRiskState {
    /// Risk policy active when the snapshot was written.
    pub config: RiskConfig,
    /// Realized loss accumulated in the session/day.
    pub daily_loss_cents: Cents,
    /// Number of unsettled hedges.
    pub open_trades: u32,
    /// Reserved capital by venue.
    pub bankroll: HashMap<VenueId, Cents>,
    /// Orders whose submission may have reached a venue.
    pub in_flight: Vec<InFlightOrder>,
}

/// Order persisted before network submission for crash recovery.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct InFlightOrder {
    /// Idempotency key.
    pub client_order_id: [u8; 16],
    /// Venue order payload.
    pub leg: PersistedExecLeg,
    /// Persisted wall-clock creation time.
    pub created_at_ms: u64,
    /// Venue order ID once acknowledged.
    pub venue_order_id: Option<String>,
}

/// Serde-safe execution leg with integer price representation.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PersistedExecLeg {
    /// Venue identifier.
    pub venue: VenueId,
    /// Venue-specific instrument.
    pub instrument: crate::VenueInstrumentRef,
    /// Limit price in parts per million.
    pub limit_price_ppm: u32,
    /// Requested stake in cents.
    pub stake_cents: Cents,
    /// Idempotency key.
    pub client_order_id: [u8; 16],
}

impl From<&ExecLeg> for PersistedExecLeg {
    fn from(leg: &ExecLeg) -> Self {
        Self {
            venue: leg.venue,
            instrument: leg.instrument.clone(),
            limit_price_ppm: leg.limit_price.ppm(),
            stake_cents: leg.stake_cents,
            client_order_id: leg.client_order_id,
        }
    }
}

impl From<&RiskGate> for DurableRiskState {
    fn from(gate: &RiskGate) -> Self {
        Self {
            config: gate.config,
            daily_loss_cents: gate.daily_loss_cents,
            open_trades: gate.open_trades,
            bankroll: gate.bankroll_snapshot(),
            in_flight: Vec::new(),
        }
    }
}

impl DurableRiskState {
    /// Restore a risk gate from a committed snapshot.
    pub fn restore(&self) -> RiskGate {
        RiskGate::from_durable(
            self.config,
            self.daily_loss_cents,
            self.open_trades,
            self.bankroll.iter().map(|(&venue, &cents)| (venue, cents)),
        )
    }
}

/// Atomic JSON-file store for durable execution state.
#[derive(Debug, Clone)]
pub struct RiskStateStore {
    path: PathBuf,
}

impl RiskStateStore {
    /// Create a store targeting `path`.
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }
    /// Read the last committed state.
    pub fn load(&self) -> Result<DurableRiskState, String> {
        let bytes =
            fs::read(&self.path).map_err(|e| format!("read {}: {e}", self.path.display()))?;
        serde_json::from_slice(&bytes).map_err(|e| format!("decode {}: {e}", self.path.display()))
    }
    /// Commit state via a sibling temporary file and rename.
    pub fn save(&self, state: &DurableRiskState) -> Result<(), String> {
        if let Some(parent) = self.path.parent().filter(|p| !p.as_os_str().is_empty()) {
            fs::create_dir_all(parent).map_err(|e| format!("create {}: {e}", parent.display()))?;
        }
        let temp = self.path.with_extension("tmp");
        fs::write(
            &temp,
            serde_json::to_vec_pretty(state).map_err(|e| format!("encode state: {e}"))?,
        )
        .map_err(|e| format!("write {}: {e}", temp.display()))?;
        fs::rename(&temp, &self.path).map_err(|e| format!("commit {}: {e}", self.path.display()))
    }
    /// Expose the configured path for diagnostics.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Persist a risk-gate checkpoint without discarding recorded in-flight
    /// orders. A plain `save(From<&gate>)` would wipe crash-recovery state on
    /// every checkpoint, so this merges the live fields over what is stored.
    pub fn checkpoint(&self, gate: &RiskGate) -> Result<DurableRiskState, String> {
        let mut state = self.load().unwrap_or_else(|_| DurableRiskState::from(gate));
        state.config = gate.config;
        state.daily_loss_cents = gate.daily_loss_cents;
        state.open_trades = gate.open_trades;
        state.bankroll = gate.bankroll_snapshot();
        self.save(&state)?;
        Ok(state)
    }

    /// Persist an order before network submission.
    pub fn register_inflight(&self, order: InFlightOrder) -> Result<DurableRiskState, String> {
        let mut state = self.load().unwrap_or_else(|_| DurableRiskState {
            config: RiskConfig::default(),
            daily_loss_cents: 0,
            open_trades: 0,
            bankroll: HashMap::new(),
            in_flight: Vec::new(),
        });
        state
            .in_flight
            .retain(|existing| existing.client_order_id != order.client_order_id);
        state.in_flight.push(order);
        self.save(&state)?;
        Ok(state)
    }

    /// Persist an acknowledgement, making restart reconciliation idempotent.
    pub fn acknowledge(
        &self,
        client_order_id: [u8; 16],
        venue_order_id: String,
    ) -> Result<DurableRiskState, String> {
        let mut state = self.load()?;
        if let Some(order) = state
            .in_flight
            .iter_mut()
            .find(|order| order.client_order_id == client_order_id)
        {
            order.venue_order_id = Some(venue_order_id);
        }
        self.save(&state)?;
        Ok(state)
    }
}

/// Fixed-window limiter for outbound venue requests.
#[derive(Debug, Clone)]
pub struct RateLimiter {
    capacity: u32,
    remaining: u32,
    window: Duration,
    started: Instant,
}
impl RateLimiter {
    /// Permit `capacity` requests per `window`.
    pub fn new(capacity: u32, window: Duration) -> Self {
        Self {
            capacity,
            remaining: capacity,
            window,
            started: Instant::now(),
        }
    }
    /// Consume one token, returning false when the caller must wait.
    pub fn allow(&mut self) -> bool {
        if self.started.elapsed() >= self.window {
            self.started = Instant::now();
            self.remaining = self.capacity;
        }
        if self.remaining == 0 {
            return false;
        }
        self.remaining -= 1;
        true
    }
}

/// Feed-health circuit breaker that suppresses execution after stale input.
#[derive(Debug, Clone)]
pub struct FeedCircuitBreaker {
    stale_after: Duration,
    last_event: Instant,
    tripped: bool,
}
impl FeedCircuitBreaker {
    /// Create a breaker that trips after `stale_after` without feed input.
    pub fn new(stale_after: Duration) -> Self {
        Self {
            stale_after,
            last_event: Instant::now(),
            tripped: false,
        }
    }
    /// Record valid feed input and clear the breaker.
    pub fn observe(&mut self) {
        self.last_event = Instant::now();
        self.tripped = false;
    }
    /// Return whether execution is currently blocked.
    pub fn blocked(&mut self) -> bool {
        if self.last_event.elapsed() >= self.stale_after {
            self.tripped = true;
        }
        self.tripped
    }
    /// Force the breaker open.
    pub fn trip(&mut self) {
        self.tripped = true;
    }
}

/// Durable reconciliation state keyed by client and venue order IDs.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct ReconciliationLedger {
    /// Known orders.
    pub orders: HashMap<String, ReconciledOrder>,
    /// Settled authoritative profit.
    pub realized_profit_cents: Cents,
    /// Authoritative fees.
    pub fees_paid_cents: Cents,
    /// Fingerprints of fill events already folded in. Fill streams are
    /// at-least-once; an exact replay must never double-count money.
    #[serde(default)]
    pub applied_fills: HashSet<String>,
}
/// One order's lifecycle state.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ReconciledOrder {
    /// Client idempotency key.
    pub client_order_id: [u8; 16],
    /// Venue order ID.
    pub venue_order_id: Option<String>,
    /// Requested stake.
    pub requested_stake_cents: Cents,
    /// Filled stake.
    pub filled_stake_cents: Cents,
    /// open, settled, or unwound.
    pub status: String,
}
/// Private fill or settlement event.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FillEvent {
    /// Client id when supplied.
    pub client_order_id: Option<[u8; 16]>,
    /// Venue order ID.
    pub venue_order_id: String,
    /// Authoritative fill.
    pub filled_stake_cents: Cents,
    /// Authoritative fee.
    pub fee_cents: Cents,
    /// Final PnL, if settled.
    pub realized_profit_cents: Option<Cents>,
    /// Venue lifecycle state.
    pub status: String,
}
impl ReconciliationLedger {
    /// Register before submission so crashes can reconcile by client ID.
    pub fn register(&mut self, order: InFlightOrder) {
        self.orders.insert(
            hex_id(&order.client_order_id),
            ReconciledOrder {
                client_order_id: order.client_order_id,
                venue_order_id: order.venue_order_id,
                requested_stake_cents: order.leg.stake_cents,
                filled_stake_cents: 0,
                status: "open".into(),
            },
        );
    }
    /// Apply an idempotent private fill update.
    pub fn apply_fill(&mut self, fill: FillEvent) -> Result<(), String> {
        let key = self
            .orders
            .iter()
            .find(|(_, order)| {
                order.venue_order_id.as_deref() == Some(fill.venue_order_id.as_str())
                    || fill.client_order_id == Some(order.client_order_id)
            })
            .map(|(key, _)| key.clone())
            .ok_or_else(|| format!("unknown order {}", fill.venue_order_id))?;
        // A replayed event is the venue restating what we already recorded;
        // folding it in again would fabricate fees or profit.
        let fingerprint = format!(
            "{}|{}|{}|{}|{}|{}",
            key,
            hex_id(&fill.client_order_id.unwrap_or_default()),
            fill.venue_order_id,
            fill.filled_stake_cents,
            fill.fee_cents,
            match fill.realized_profit_cents {
                Some(profit) => format!("profit={profit}"),
                None => "unsettled".to_owned(),
            },
        );
        if !self.applied_fills.insert(fingerprint) {
            return Ok(());
        }
        let order = self.orders.get_mut(&key).expect("key found above");
        order.venue_order_id = Some(fill.venue_order_id);
        order.filled_stake_cents = fill.filled_stake_cents;
        order.status = fill.status;
        self.fees_paid_cents = self.fees_paid_cents.saturating_add(fill.fee_cents);
        if let Some(profit) = fill.realized_profit_cents {
            self.realized_profit_cents = self.realized_profit_cents.saturating_add(profit);
        }
        Ok(())
    }

    /// Consume a private JSON event after the venue-specific parser has decoded it.
    pub fn apply_raw_fill(&mut self, fill: FillEvent) -> Result<(), String> {
        self.apply_fill(fill)
    }

    /// Serialize the authoritative ledger for the dashboard/ledger worker.
    pub fn to_json(&self) -> Result<String, String> {
        serde_json::to_string(self).map_err(|e| format!("encode reconciliation ledger: {e}"))
    }
}
fn hex_id(id: &[u8; 16]) -> String {
    id.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::VenueInstrumentRef;
    use arbkit_core::Prob;
    fn leg() -> ExecLeg {
        ExecLeg {
            venue: 1,
            instrument: VenueInstrumentRef::Kalshi("KX".into()),
            limit_price: Prob::from_cents(50).unwrap(),
            stake_cents: 100,
            client_order_id: [4; 16],
        }
    }
    #[test]
    fn state_and_reconciliation_round_trip() {
        let path = std::env::temp_dir().join(format!("arbkit-state-{}", std::process::id()));
        let store = RiskStateStore::new(&path);
        let gate = RiskGate::new(RiskConfig::default(), [(1, 500)]);
        let state = DurableRiskState::from(&gate);
        store.save(&state).unwrap();
        assert_eq!(store.load().unwrap(), state);
        let _ = fs::remove_file(path);
        let mut ledger = ReconciliationLedger::default();
        ledger.register(InFlightOrder {
            client_order_id: [4; 16],
            leg: PersistedExecLeg::from(&leg()),
            created_at_ms: 0,
            venue_order_id: Some("v1".into()),
        });
        ledger
            .apply_fill(FillEvent {
                client_order_id: Some([4; 16]),
                venue_order_id: "v1".into(),
                filled_stake_cents: 80,
                fee_cents: 2,
                realized_profit_cents: None,
                status: "open".into(),
            })
            .unwrap();
        assert_eq!(
            ledger.orders["04040404040404040404040404040404"].filled_stake_cents,
            80
        );
        assert_eq!(ledger.fees_paid_cents, 2);
    }

    #[test]
    fn replayed_fill_events_are_counted_once() {
        let mut ledger = ReconciliationLedger::default();
        ledger.register(InFlightOrder {
            client_order_id: [9; 16],
            leg: PersistedExecLeg::from(&leg()),
            created_at_ms: 0,
            venue_order_id: None,
        });
        let fill = |profit: Option<i64>| FillEvent {
            client_order_id: Some([9; 16]),
            venue_order_id: "v-replay".into(),
            filled_stake_cents: 80,
            fee_cents: 3,
            realized_profit_cents: profit,
            status: "settled".into(),
        };

        ledger.apply_fill(fill(Some(120))).unwrap();
        // The venue redelivers the same settlement (at-least-once stream).
        ledger.apply_fill(fill(Some(120))).unwrap();

        assert_eq!(
            ledger.fees_paid_cents, 3,
            "replay must not double-count fees"
        );
        assert_eq!(ledger.realized_profit_cents, 120);
        assert_eq!(ledger.applied_fills.len(), 1);

        // A genuinely new event still accumulates.
        ledger.apply_fill(fill(Some(80))).unwrap();
        assert_eq!(ledger.realized_profit_cents, 200);
        assert_eq!(ledger.fees_paid_cents, 6);
    }
}
