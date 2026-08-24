//! Integer-only live proof report and paper/live tape comparison.

use arbkit_core::Cents;
use serde::{Deserialize, Serialize};
/// Counters emitted for one live session.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LiveProofReport {
    /// Number of signals submitted to the execution gate.
    pub attempted_arbs: u64,
    /// Fully filled hedges.
    pub live_fills: u64,
    /// Failed hedges, including unwinds.
    pub live_phantoms: u64,
    /// Number of unwind operations.
    pub unwinds: u64,
    /// Detection-time theoretical profit.
    pub theoretical_profit_cents: Cents,
    /// Settled authoritative profit.
    pub realized_profit_cents: Cents,
    /// Slippage from detection to execution.
    pub slippage_cents: Cents,
    /// Fees charged by venues.
    pub fees_paid_cents: Cents,
    /// Filled stake used as the ROI denominator.
    pub filled_stake_cents: Cents,
}

impl LiveProofReport {
    /// Realized ROI in basis points, floored toward negative infinity.
    pub fn realized_roi_bps(&self) -> i64 {
        if self.filled_stake_cents <= 0 {
            return 0;
        }
        (self.realized_profit_cents as i128 * 10_000).div_euclid(self.filled_stake_cents as i128)
            as i64
    }

    /// Serialize an immutable proof artifact.
    pub fn to_json(&self) -> serde_json::Result<String> {
        serde_json::to_string_pretty(self)
    }
}

/// Compare paper and live ROI measured against the same recorded tape.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TapeComparison {
    /// Paper ROI in basis points.
    pub paper_roi_bps: i64,
    /// Live ROI in basis points.
    pub live_roi_bps: i64,
    /// Absolute difference in basis points.
    pub roi_delta_bps: i64,
    /// Whether the difference is inside the requested tolerance.
    pub within_tolerance: bool,
}

/// Compare two reports using integer basis points.
pub fn compare_tape(
    paper: &LiveProofReport,
    live: &LiveProofReport,
    tolerance_bps: i64,
) -> TapeComparison {
    let paper_roi_bps = paper.realized_roi_bps();
    let live_roi_bps = live.realized_roi_bps();
    let roi_delta_bps = live_roi_bps.saturating_sub(paper_roi_bps);
    TapeComparison {
        paper_roi_bps,
        live_roi_bps,
        roi_delta_bps,
        within_tolerance: roi_delta_bps.abs() <= tolerance_bps.max(0),
    }
}

// ---------------------------------------------------------------------------
// Same-tape paper replay (feature `paper-replay`).
//
// The execution-boundary "tape" is one NDJSON record per detected signal,
// frozen at detection time: quoted plan, per-leg stakes and payouts, and the
// venue arrival state the runner observed. Replaying those records through
// `arbkit-sim` reproduces what the paper simulator would have done with the
// identical sequence, so a session's live report can be compared against
// paper on equal terms — same inputs, same order, integer math throughout.
// ---------------------------------------------------------------------------

/// One leg of an [`OccurrenceRecord`], as frozen by the runner at detection.
#[cfg(feature = "paper-replay")]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OccurrenceLeg {
    /// Venue identifier.
    pub venue: u16,
    /// Outcome this leg backs.
    pub outcome: u32,
    /// Detection-time quoted price in parts per million.
    pub quoted_ppm: u32,
    /// Stake fee in basis points charged by the venue on entry.
    #[serde(default)]
    pub fee_bps_stake: u32,
    /// Maximum stake the quote could absorb, in cents.
    pub capacity_cents: Cents,
    /// Smallest stake step, in cents.
    pub increment_cents: Cents,
    /// Stake allocated to this leg, in cents.
    pub stake_cents: Cents,
    /// Gross payout if this leg's outcome wins, stake included, in cents.
    pub payout_cents: Cents,
    /// Price observed on arrival, in ppm; `null` when no quote remained.
    pub arrival_price_ppm: Option<u32>,
    /// Depth still resting on arrival, in cents.
    pub arrival_depth_cents: Cents,
}

/// One detected signal, frozen at detection time for same-tape replay.
#[cfg(feature = "paper-replay")]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OccurrenceRecord {
    /// Runner-side sequence number.
    pub seq: u64,
    /// Detection timestamp in nanoseconds.
    pub detection_timestamp_ns: u64,
    /// Detected edge in basis points.
    pub edge_bps: u32,
    /// Worst-case guaranteed profit at lock time, in cents.
    pub worst_case_profit_cents: Cents,
    /// Hedge legs, exactly as planned.
    pub legs: Vec<OccurrenceLeg>,
}

/// Replay an occurrence tape through the paper simulator and reduce it to a
/// [`LiveProofReport`], directly comparable against the live artifact for the
/// same tape.
#[cfg(feature = "paper-replay")]
pub fn replay_paper_tape(records: &[OccurrenceRecord]) -> Result<LiveProofReport, String> {
    use arbkit_core::{Allocation, Fee, Leg, Prob, Signal};
    use arbkit_sim::{LatencyModel, LatencyProfile, Simulator, MAX_SIM_LEGS};

    let mut simulator = Simulator::new(LatencyModel::new(LatencyProfile::ZERO));

    for record in records {
        let leg_count = record.legs.len();
        if !(2..=MAX_SIM_LEGS).contains(&leg_count) {
            return Err(format!(
                "occurrence {}: expected 2..={} legs, found {leg_count}",
                record.seq, MAX_SIM_LEGS
            ));
        }

        let mut allocations = [Allocation {
            leg: 0,
            stake: 0,
            payout: 0,
        }; MAX_SIM_LEGS];
        let mut legs = Vec::with_capacity(leg_count);
        let mut arrival_prices = Vec::with_capacity(leg_count);
        let mut arrival_depths = Vec::with_capacity(leg_count);
        let mut total_stake = 0;

        for (index, leg) in record.legs.iter().enumerate() {
            allocations[index] = Allocation {
                leg: index,
                stake: leg.stake_cents,
                payout: leg.payout_cents,
            };
            total_stake += leg.stake_cents;
            legs.push(Leg {
                venue: leg.venue,
                outcome: leg.outcome,
                quoted: Prob::from_ppm(leg.quoted_ppm)
                    .map_err(|e| format!("occurrence {} leg {index}: {e}", record.seq))?,
                fee: Fee::StakeFeeBps(leg.fee_bps_stake),
                capacity: leg.capacity_cents,
                increment: leg.increment_cents,
            });
            arrival_prices.push(
                leg.arrival_price_ppm
                    .map(Prob::from_ppm)
                    .transpose()
                    .map_err(|e| format!("occurrence {} leg {index} arrival: {e}", record.seq))?,
            );
            arrival_depths.push(leg.arrival_depth_cents);
        }

        let signal = Signal::from_raw_parts(
            allocations,
            leg_count as u8,
            // Overround is detector metadata; replay does not re-detect.
            0,
            total_stake,
            record.worst_case_profit_cents,
            record.edge_bps,
        );

        simulator
            .simulate_with_quotes(
                record.detection_timestamp_ns,
                &signal,
                &legs,
                &arrival_prices,
                &arrival_depths,
            )
            .map_err(|e| format!("occurrence {}: {e}", record.seq))?;
    }

    let stats = simulator.stats();
    Ok(LiveProofReport {
        attempted_arbs: stats.attempted,
        // Proportional fills are positions too. `total_phantoms` already
        // includes broken-leg classifications: a moved price on one leg
        // leaves the other side filled and unhedged, which is a phantom
        // with directional damage, not a separate category.
        live_fills: stats.clean_fills + stats.proportional_fills,
        live_phantoms: stats.total_phantoms,
        // Paper replay never transmits an order, so nothing is unwound.
        unwinds: 0,
        theoretical_profit_cents: stats.total_expected_profit_cents,
        realized_profit_cents: stats.total_realized_profit_cents,
        slippage_cents: stats.total_slippage_cents,
        fees_paid_cents: stats.total_fees_paid_cents,
        filled_stake_cents: stats.total_filled_stake_cents,
    })
}
