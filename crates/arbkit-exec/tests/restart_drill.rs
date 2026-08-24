//! Restart drill (HJ-150): a crashed session's durable state must restore
//! the gate exactly, refuse unreconciled in-flight orders, and reconcile
//! settlements by client order id with idempotent acknowledgement.
//!
//! The drill drives the same library pieces `prod_trader` wires —
//! `RiskStateStore` (`register_inflight`/`acknowledge`/`clear_inflight`/
//! `checkpoint`), `RiskGate::from_durable`, and `Reconciler` — through the
//! production sequence: persist before submission, acknowledge after,
//! crash, reload, refuse-or-resume, reconcile, clear.

use arbkit_core::{Cents, VenueId};
use arbkit_exec::{
    InFlightOrder, PersistedExecLeg, Reconciler, ReconciliationLedger, RiskConfig, RiskGate,
    RiskStateStore, VenueInstrumentRef,
};
use arbkit_match::VenueRegistry;
use std::collections::HashMap;

/// The policy the crashed session was operating under.
fn stored_policy() -> RiskConfig {
    RiskConfig {
        max_stake_per_leg_cents: 3_000,
        max_daily_loss_cents: 40_000,
        max_open_trades: 2,
        min_edge_bps: 50,
        kill_switch: true,
    }
}

/// What the operator's environment says on restart — deliberately different.
fn env_policy() -> RiskConfig {
    RiskConfig {
        max_stake_per_leg_cents: 9_999,
        max_daily_loss_cents: 900_000,
        max_open_trades: 9,
        min_edge_bps: 5,
        kill_switch: false,
    }
}

/// The runner's limits-only restore: durable policy wins over env so a
/// restart cannot silently widen caps mid-day; kill-switch posture follows
/// this run's environment.
fn effective_config(stored: &RiskConfig, env: &RiskConfig) -> RiskConfig {
    RiskConfig {
        max_stake_per_leg_cents: stored.max_stake_per_leg_cents,
        max_daily_loss_cents: stored.max_daily_loss_cents,
        max_open_trades: stored.max_open_trades,
        min_edge_bps: stored.min_edge_bps,
        kill_switch: env.kill_switch,
    }
}

fn order(id: [u8; 16], venue: VenueId) -> InFlightOrder {
    InFlightOrder {
        client_order_id: id,
        leg: PersistedExecLeg {
            venue,
            instrument: VenueInstrumentRef::Kalshi("KXNBAGAME-26AUG181930BOSLAL-BOS".into()),
            limit_price_ppm: 480_000,
            stake_cents: 2_500,
            client_order_id: id,
        },
        created_at_ms: 1_756_000_000_000,
        venue_order_id: None,
    }
}

#[test]
fn restart_restores_gate_exactly_and_reconciles_idempotently() {
    let dir = std::env::temp_dir().join(format!("arbkit-restart-drill-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let store = RiskStateStore::new(dir.join("prod-risk-state.json"));

    // ---- Session 1 end-state: one hedge lost 300c today, another is open
    // with both legs registered before submission and the Kalshi leg
    // already acknowledged by the venue. This is exactly what the runner's
    // checkpoint/register/acknowledge calls leave behind. ----
    let bankroll: HashMap<VenueId, Cents> = HashMap::from([
        (VenueRegistry::KALSHI, 10_000),
        (VenueRegistry::POLYMARKET, 10_000),
    ]);
    store
        .save(&arbkit_exec::DurableRiskState {
            config: stored_policy(),
            daily_loss_cents: 300,
            open_trades: 1,
            bankroll: bankroll.clone(),
            in_flight: Vec::new(),
        })
        .unwrap();
    let kalshi_id = [0xA1; 16];
    let poly_id = [0xB2; 16];
    store
        .register_inflight(order(kalshi_id, VenueRegistry::KALSHI))
        .unwrap();
    store
        .register_inflight(order(poly_id, VenueRegistry::POLYMARKET))
        .unwrap();
    store.acknowledge(kalshi_id, "venue-a1".into()).unwrap();
    // ---- CRASH -----------------------------------------------------------

    // ---- Session 2: reload and inspect what survived. --------------------
    let loaded = store.load().expect("crash left readable state");
    assert_eq!(loaded.config, stored_policy());
    assert_eq!(loaded.daily_loss_cents, 300);
    assert_eq!(loaded.open_trades, 1);
    assert_eq!(loaded.bankroll, bankroll);
    assert_eq!(loaded.in_flight.len(), 2);
    assert_eq!(
        loaded
            .in_flight
            .iter()
            .find(|o| o.client_order_id == kalshi_id)
            .unwrap()
            .venue_order_id
            .as_deref(),
        Some("venue-a1")
    );
    assert!(
        loaded
            .in_flight
            .iter()
            .find(|o| o.client_order_id == poly_id)
            .unwrap()
            .venue_order_id
            .is_none(),
        "the unacknowledged leg has no venue id yet"
    );

    // Live mode refuses a restart while anything is unaccounted for.
    assert!(!loaded.in_flight.is_empty());

    // Gate restoration is exact — including the limits-only policy merge.
    let config = effective_config(&loaded.config, &env_policy());
    assert_eq!(config.max_stake_per_leg_cents, 3_000);
    assert_eq!(config.max_daily_loss_cents, 40_000);
    assert_eq!(config.max_open_trades, 2);
    assert_eq!(config.min_edge_bps, 50);
    assert!(!config.kill_switch, "posture follows the environment");
    let mut gate = RiskGate::from_durable(
        config,
        loaded.daily_loss_cents,
        loaded.open_trades,
        loaded.bankroll.iter().map(|(&v, &c)| (v, c)),
    );
    assert_eq!(gate.bankroll_snapshot(), bankroll);

    // Re-seed and reconcile exactly like the runner's poll loop: the
    // acknowledged leg settles; the unacknowledged leg waits.
    let mut reconciler = Reconciler::new(ReconciliationLedger::default());
    reconciler.seed_in_flight(loaded.in_flight.clone());
    let source = TerminalFill {
        by_venue_order: HashMap::from([(
            "venue-a1".to_string(),
            FillShape {
                filled_stake_cents: 2_500,
                fee_cents: 4,
                realized_profit_cents: Some(120),
            },
        )]),
    };
    let settled = reconciler.reconcile(&source).unwrap();
    assert_eq!(settled.len(), 1, "the acknowledged leg settles");
    assert_eq!(settled[0].client_order_id, kalshi_id);
    assert_eq!(settled[0].realized_profit_cents, Some(120));
    gate.settle(settled[0].realized_profit_cents.unwrap_or(0));

    // Idempotent: re-polling applies nothing further.
    assert!(reconciler.reconcile(&source).unwrap().is_empty());
    assert_eq!(reconciler.ledger().realized_profit_cents, 120);

    // Settled means cleared: the durable ghost must go, not linger and trip
    // every future live restart.
    store.clear_inflight(kalshi_id).unwrap();
    assert_eq!(store.load().unwrap().in_flight.len(), 1);

    // THE core acceptance line: a plain checkpoint must not erase the
    // remaining crash-recovery state even though the fresh gate knows
    // nothing about it.
    store.checkpoint(&gate).unwrap();
    let after = store.load().unwrap();
    assert_eq!(
        after
            .in_flight
            .iter()
            .map(|o| o.client_order_id)
            .collect::<Vec<_>>(),
        vec![poly_id],
        "checkpoint preserves unacknowledged recovery state"
    );
    assert_eq!(after.daily_loss_cents, 300);

    // Operator resolves the last order; the refusal predicate clears.
    store.clear_inflight(poly_id).unwrap();
    assert!(store.load().unwrap().in_flight.is_empty());

    let _ = std::fs::remove_dir_all(&dir);
}

struct FillShape {
    filled_stake_cents: Cents,
    fee_cents: Cents,
    realized_profit_cents: Option<Cents>,
}

struct TerminalFill {
    by_venue_order: HashMap<String, FillShape>,
}

impl arbkit_exec::SettlementSource for TerminalFill {
    fn poll(&self, order: &InFlightOrder) -> Result<Option<arbkit_exec::FillEvent>, String> {
        let Some(venue_id) = order.venue_order_id.as_deref() else {
            return Ok(None);
        };
        Ok(self
            .by_venue_order
            .get(venue_id)
            .map(|fill| arbkit_exec::FillEvent {
                client_order_id: Some(order.client_order_id),
                venue_order_id: venue_id.to_string(),
                filled_stake_cents: fill.filled_stake_cents,
                fee_cents: fill.fee_cents,
                realized_profit_cents: fill.realized_profit_cents,
                status: "settled".to_string(),
            }))
    }
}
