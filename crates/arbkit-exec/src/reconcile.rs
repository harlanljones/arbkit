//! Runner-side settlement reconciliation.
//!
//! Venue order submission and venue settlement are separate facts. A hedge
//! opens on submission and inverts only when the venue reports a terminal
//! lifecycle ("settled", "unwound") and an authoritative fill. This module
//! owns that second half: it folds venue fill events into the durable
//! [`ReconciliationLedger`] idempotently and closes an in-flight order the
//! first time the venue reports a terminal status.
//!
//! It is deliberately I/O-free. Venue access hides behind [`SettlementSource`],
//! so the reconciler is unit-testable with a scripted source and the runner
//! can swap a poll-based source for a private-fill subscription without
//! touching the reconciliation logic.

use crate::state::{FillEvent, InFlightOrder, ReconciliationLedger};
use arbkit_core::Cents;

/// Hex-encode an idempotency key, matching the ledger's key convention.
fn hex_id(id: &[u8; 16]) -> String {
    id.iter().map(|b| format!("{b:02x}")).collect()
}

/// Authoritative settlement state for one order, produced the first time the
/// venue reports a terminal lifecycle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Settlement {
    /// Idempotency key the executor committed before network submission.
    pub client_order_id: [u8; 16],
    /// Venue order ID once acknowledged.
    pub venue_order_id: String,
    /// Authoritative filled stake.
    pub filled_stake_cents: Cents,
    /// Authoritative realized profit, or `None` while unknown.
    pub realized_profit_cents: Option<Cents>,
    /// Authoritative fees.
    pub fees_paid_cents: Cents,
    /// Terminal venue status (`settled` or `unwound`).
    pub status: String,
}

/// Source of authoritative fill/settlement state for one in-flight order.
///
/// Implementations own all venue I/O. Returning `Ok(None)` means the source
/// has no opinion yet (order still unknown to it); returning a
/// [`FillEvent`] is the venue restating everything it knows so far.
pub trait SettlementSource {
    /// Poll one order's current authoritative state.
    fn poll(&self, order: &InFlightOrder) -> Result<Option<FillEvent>, String>;
}

/// Returns whether a venue status has reached a terminal settlement.
pub fn is_terminal_status(status: &str) -> bool {
    status.eq_ignore_ascii_case("settled")
        || status.eq_ignore_ascii_case("unwound")
        || status.eq_ignore_ascii_case("canceled")
}

/// In-memory reconciliation state seeded from and checkpointed to durable
/// storage.
#[derive(Debug, Clone, Default)]
pub struct Reconciler {
    ledger: ReconciliationLedger,
    in_flight: Vec<InFlightOrder>,
}

impl Reconciler {
    /// Build an empty reconciler.
    pub fn new(ledger: ReconciliationLedger) -> Self {
        Self {
            ledger,
            in_flight: Vec::new(),
        }
    }

    /// Rebuild the in-flight set from a durable snapshot so a restart does not
    /// orphan orders that were in flight when the process stopped. The ledger
    /// is re-seeded at the same time so its orders map stays consistent.
    pub fn seed_in_flight(&mut self, orders: Vec<InFlightOrder>) {
        for order in &orders {
            self.ledger.register(order.clone());
        }
        self.in_flight = orders;
    }

    /// The durable reconciliation ledger.
    pub fn ledger(&self) -> &ReconciliationLedger {
        &self.ledger
    }

    /// Mutable access to the durable reconciliation ledger.
    pub fn ledger_mut(&mut self) -> &mut ReconciliationLedger {
        &mut self.ledger
    }

    /// The orders still awaiting terminal settlement.
    pub fn in_flight(&self) -> &[InFlightOrder] {
        &self.in_flight
    }

    /// Register an order before submission so a crash can reconcile by client
    /// id. Re-registering the same id replaces the prior entry.
    pub fn register(&mut self, order: InFlightOrder) {
        self.ledger.register(order.clone());
        self.in_flight
            .retain(|existing| existing.client_order_id != order.client_order_id);
        self.in_flight.push(order);
    }

    /// Record a venue acknowledgement, making restart reconciliation
    /// idempotent by venue order id.
    pub fn acknowledge(&mut self, client_order_id: [u8; 16], venue_order_id: String) {
        for order in &mut self.in_flight {
            if order.client_order_id == client_order_id {
                order.venue_order_id = Some(venue_order_id.clone());
            }
        }
        if let Some(order) = self.ledger.orders.get_mut(&hex_id(&client_order_id)) {
            order.venue_order_id = Some(venue_order_id);
        }
    }

    /// Pull each acknowledged in-flight order and fold any fill events into
    /// the ledger. Returns the set of orders that reached a terminal
    /// status for the first time; these are removed from the in-flight set.
    ///
    /// A fill is applied idempotently: an at-least-once replay cannot
    /// double-count fees or profit. An unacknowledged order (no venue id yet)
    /// is left alone, and a source error leaves the order pending rather than
    /// marking it settled.
    pub fn reconcile(&mut self, source: &impl SettlementSource) -> Result<Vec<Settlement>, String> {
        let mut settled = Vec::new();
        let mut remaining = Vec::with_capacity(self.in_flight.len());
        for order in self.in_flight.drain(..) {
            let Some(venue_order_id) = order.venue_order_id.as_deref() else {
                // Not yet acknowledged; poll again next tick.
                remaining.push(order);
                continue;
            };
            if venue_order_id.is_empty() {
                remaining.push(order);
                continue;
            }
            match source.poll(&order) {
                Ok(Some(fill)) => {
                    self.ledger.apply_fill(fill.clone())?;
                    if is_terminal_status(&fill.status) {
                        settled.push(Settlement {
                            client_order_id: order.client_order_id,
                            venue_order_id: fill.venue_order_id,
                            filled_stake_cents: fill.filled_stake_cents,
                            realized_profit_cents: fill.realized_profit_cents,
                            fees_paid_cents: fill.fee_cents,
                            status: fill.status,
                        });
                    } else {
                        remaining.push(order);
                    }
                }
                Ok(None) => remaining.push(order),
                Err(_) => remaining.push(order),
            }
        }
        self.in_flight = remaining;
        Ok(settled)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::InFlightOrder;
    use crate::VenueInstrumentRef;

    fn order(id: [u8; 16]) -> InFlightOrder {
        InFlightOrder {
            client_order_id: id,
            leg: crate::PersistedExecLeg {
                venue: 1,
                instrument: VenueInstrumentRef::Kalshi("KX".into()),
                limit_price_ppm: 500_000,
                stake_cents: 100,
                client_order_id: id,
            },
            created_at_ms: 1,
            venue_order_id: Some(format!("o-{}", id[0])),
        }
    }

    fn fill(client: [u8; 16], venue: &str, status: &str, profit: Option<i64>) -> FillEvent {
        FillEvent {
            client_order_id: Some(client),
            venue_order_id: venue.to_string(),
            filled_stake_cents: 100,
            fee_cents: 2,
            realized_profit_cents: profit,
            status: status.to_string(),
        }
    }

    struct Source<'a> {
        by_id: Vec<(&'a str, FillEvent)>,
    }
    impl SettlementSource for Source<'_> {
        fn poll(&self, order: &InFlightOrder) -> Result<Option<FillEvent>, String> {
            let venue = order.venue_order_id.as_deref().unwrap_or_default();
            Ok(self
                .by_id
                .iter()
                .find(|(id, _)| *id == venue)
                .map(|(_, event)| event.clone()))
        }
    }

    #[test]
    fn settles_an_order_once_when_terminal() {
        let mut r = Reconciler::new(ReconciliationLedger::default());
        r.register(order([1; 16]));
        let source = Source {
            by_id: vec![("o-1", fill([1; 16], "o-1", "settled", Some(20)))],
        };
        let settled = r.reconcile(&source).unwrap();
        assert_eq!(settled.len(), 1);
        assert_eq!(settled[0].realized_profit_cents, Some(20));
        assert_eq!(settled[0].status, "settled");
        assert!(r.in_flight().is_empty());
        assert_eq!(r.ledger().realized_profit_cents, 20);
        assert_eq!(r.ledger().fees_paid_cents, 2);
    }

    #[test]
    fn idempotent_replays_do_not_double_count() {
        let mut r = Reconciler::new(ReconciliationLedger::default());
        r.register(order([2; 16]));
        let source = Source {
            by_id: vec![("o-2", fill([2; 16], "o-2", "settled", Some(20)))],
        };
        // First pull closes the order; a replay is what at-least-once delivery
        // looks like, so re-running on a fresh in-flight seed must not add.
        r.reconcile(&source).unwrap();
        assert_eq!(r.ledger().realized_profit_cents, 20);
        r.reconcile(&source).unwrap();
        assert_eq!(r.ledger().realized_profit_cents, 20);
        assert_eq!(r.ledger().fees_paid_cents, 2);
    }

    #[test]
    fn rejects_a_duplicate_fill_fingerprint() {
        let mut r = Reconciler::new(ReconciliationLedger::default());
        r.register(order([3; 16]));
        let source = Source {
            by_id: vec![("o-3", fill([3; 16], "o-3", "settled", Some(10)))],
        };
        r.reconcile(&source).unwrap();
        // Manually re-apply the same fill; the fingerprint guard drops it.
        r.ledger_mut()
            .apply_fill(fill([3; 16], "o-3", "settled", Some(10)))
            .unwrap();
        assert_eq!(r.ledger().realized_profit_cents, 10);
    }

    #[test]
    fn unacknowledged_or_still_open_orders_stay_pending() {
        let mut r = Reconciler::new(ReconciliationLedger::default());
        let mut unack = order([4; 16]);
        unack.venue_order_id = None;
        r.register(unack);
        r.register(order([5; 16]));
        let source = Source {
            by_id: vec![("o-5", fill([5; 16], "o-5", "executed", None))],
        };
        let settled = r.reconcile(&source).unwrap();
        assert!(settled.is_empty());
        // Both still pending: one unacknowledged, one still open.
        assert_eq!(r.in_flight().len(), 2);
    }

    #[test]
    fn seeding_in_flight_clears_a_restart_orphan() {
        let mut r = Reconciler::new(ReconciliationLedger::default());
        let o = order([6; 16]);
        r.seed_in_flight(vec![o]);
        let source = Source {
            by_id: vec![("o-6", fill([6; 16], "o-6", "unwound", Some(-15)))],
        };
        let settled = r.reconcile(&source).unwrap();
        assert_eq!(settled.len(), 1);
        assert_eq!(settled[0].realized_profit_cents, Some(-15));
        assert_eq!(settled[0].status, "unwound");
        assert!(r.in_flight().is_empty());
    }
}
