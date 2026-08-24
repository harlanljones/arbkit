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
