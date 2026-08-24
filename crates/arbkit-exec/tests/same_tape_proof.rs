//! Same-tape proof integration tests (HJ-65).
//!
//! Pins the proof protocol end to end: an occurrence tape replayed through
//! the paper simulator reduces to a `LiveProofReport`, `compare_tape` keeps
//! integer math honest, and a live session that diverges beyond tolerance is
//! reported as falsified rather than smoothed over.
//!
//! Fixture arithmetic (integer-exact): leg 1 backs outcome 7 at 50c with 100c
//! (payout 200c); leg 2 backs outcome 8 at 45c with 90c (payout 200c). Either
//! winner returns 200c on 190c staked — a guaranteed 10c per occurrence.

#![cfg(feature = "paper-replay")]

use arbkit_exec::{
    compare_tape, replay_paper_tape, LiveProofReport, OccurrenceLeg, OccurrenceRecord,
};

fn leg(venue: u16, outcome: u32, quoted_ppm: u32, stake: i64) -> OccurrenceLeg {
    OccurrenceLeg {
        venue,
        outcome,
        quoted_ppm,
        fee_bps_stake: 0,
        capacity_cents: 500,
        increment_cents: 1,
        stake_cents: stake,
        payout_cents: (stake * 1_000_000) / i64::from(quoted_ppm),
        arrival_price_ppm: Some(quoted_ppm),
        arrival_depth_cents: 500,
    }
}

/// Both arrivals still show the detected quote: the simulator fills clean.
fn filled_occurrence(seq: u64) -> OccurrenceRecord {
    OccurrenceRecord {
        seq,
        detection_timestamp_ns: 1_000 + seq,
        edge_bps: 526,
        worst_case_profit_cents: 10,
        legs: vec![leg(1, 7, 500_000, 100), leg(2, 8, 450_000, 90)],
    }
}

#[test]
fn parity_tape_replays_clean_and_compares_within_tolerance() {
    let tape: Vec<OccurrenceRecord> = (0..3).map(filled_occurrence).collect();
    let paper = replay_paper_tape(&tape).expect("replay");

    assert_eq!(paper.attempted_arbs, 3);
    assert_eq!(paper.live_fills, 3);
    assert_eq!(paper.live_phantoms, 0);
    assert_eq!(paper.theoretical_profit_cents, 30);
    assert_eq!(paper.realized_profit_cents, 30);
    assert_eq!(paper.slippage_cents, 0);
    assert_eq!(paper.filled_stake_cents, 570);
    assert_eq!(paper.realized_roi_bps(), 526); // floor(30 * 10^4 / 570)

    // The runner's artifact for the same tape, with one cent of fee drag.
    let mut live = paper;
    live.live_fills = 3;
    live.realized_profit_cents = 29;
    live.fees_paid_cents = 1;

    let comparison = compare_tape(&paper, &live, 50);
    assert!(comparison.within_tolerance);
    // floor(29e4/570) = 508 vs 526: an 18 bps drag inside the band.
    assert_eq!(comparison.roi_delta_bps, -18);
}

#[test]
fn divergent_live_session_is_reported_as_falsified() {
    let tape: Vec<OccurrenceRecord> = (0..3).map(filled_occurrence).collect();
    let paper = replay_paper_tape(&tape).expect("replay");

    // The live session claims the same theoretical book but settled far
    // worse — the synthetic assumption did not transfer.
    let live = LiveProofReport {
        attempted_arbs: 3,
        live_fills: 2,
        live_phantoms: 1,
        unwinds: 1,
        theoretical_profit_cents: 30,
        realized_profit_cents: -300,
        slippage_cents: 1_500,
        fees_paid_cents: 40,
        filled_stake_cents: 400,
    };

    let comparison = compare_tape(&paper, &live, 50);
    assert!(!comparison.within_tolerance, "divergence must be visible");
    assert!(comparison.roi_delta_bps < 0);
    assert!(comparison.paper_roi_bps > comparison.live_roi_bps);

    // The serialized artifact carries the verdict for the record.
    let json = serde_json::to_string(&comparison).expect("serialize");
    assert!(json.contains("\"within_tolerance\":false"));
    // Negative live ROI is still a valid result: report it, do not relabel it.
    assert!(live.realized_profit_cents < 0 && live.realized_roi_bps() < 0);
}

#[test]
fn moved_price_on_one_leg_is_a_broken_leg_not_a_clean_fill() {
    // Outcome 8's arrival repriced from 45c to 47c — worse for the backer.
    let mut slipped = filled_occurrence(9);
    slipped.legs[1].arrival_price_ppm = Some(470_000);

    let paper = replay_paper_tape(std::slice::from_ref(&slipped)).expect("replay");

    // The hedge never assembled: this is a phantom, and specifically the
    // broken-leg kind — outcome 7 filled alone, so the session is naked that
    // side and eats its full stake when the detection view fails to hold.
    assert_eq!(paper.live_phantoms, 1);
    assert_eq!(paper.live_fills, 0);
    assert_eq!(paper.filled_stake_cents, 100);
    assert_eq!(paper.realized_profit_cents, -100);
    assert_eq!(paper.slippage_cents, 110); // expected 10 minus realized -100
    assert!(paper.realized_roi_bps() < 0);
}

#[test]
fn vanished_quote_leaves_nothing_filled() {
    let mut gone = filled_occurrence(10);
    gone.legs[1].arrival_price_ppm = None;

    let paper = replay_paper_tape(std::slice::from_ref(&gone)).expect("replay");
    assert_eq!(paper.live_phantoms, 1);
    assert_eq!(paper.filled_stake_cents, 100);
    assert_eq!(paper.realized_profit_cents, -100);
}

#[test]
fn malformed_tapes_are_rejected_not_guessed() {
    let mut single_leg = filled_occurrence(0);
    single_leg.legs.pop();

    let error = replay_paper_tape(std::slice::from_ref(&single_leg))
        .expect_err("one-legged arb is not a hedge");
    assert!(error.contains("legs"), "{error}");

    let mut bad_price = filled_occurrence(1);
    bad_price.legs[0].quoted_ppm = 1_000_001;
    assert!(replay_paper_tape(std::slice::from_ref(&bad_price)).is_err());
}
