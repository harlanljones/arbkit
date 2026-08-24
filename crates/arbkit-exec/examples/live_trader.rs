//! End-to-end dry-run execution entry point.

use arbkit_core::Prob;
use arbkit_exec::{
    DryRunAdapter, ExecLeg, ExecMode, HedgedExecutor, RiskConfig, RiskGate, VenueInstrumentRef,
};
use std::env;

#[tokio::main]
async fn main() {
    let mode = env::args()
        .skip(1)
        .find_map(|arg| arg.strip_prefix("--mode=").map(str::to_owned))
        .unwrap_or_else(|| "dry-run".to_owned());
    let mode = match mode.as_str() {
        "dry-run" => ExecMode::DryRun,
        "live" => ExecMode::Live,
        other => {
            eprintln!("invalid --mode={other}; use dry-run or live");
            std::process::exit(2);
        }
    };
    let kill_switch = env::var("ARBKIT_KILL_SWITCH")
        .map(|v| v != "0")
        .unwrap_or(true);
    let config = RiskConfig {
        kill_switch,
        ..RiskConfig::default()
    };
    println!(
        "arbkit live_trader mode={mode:?} kill_switch={} min_edge_bps={}",
        config.kill_switch, config.min_edge_bps
    );
    if matches!(mode, ExecMode::Live) && config.kill_switch {
        eprintln!("live mode refused: ARBKIT_KILL_SWITCH is active");
        std::process::exit(3);
    }
    if matches!(mode, ExecMode::DryRun) {
        let mut risk = RiskGate::new(
            RiskConfig {
                kill_switch: false,
                ..config
            },
            [(1, 10_000), (2, 10_000)],
        );
        let legs = [
            ExecLeg {
                venue: 1,
                instrument: VenueInstrumentRef::Kalshi("DRY-RUN-A".into()),
                limit_price: Prob::from_cents(49).unwrap(),
                stake_cents: 100,
                client_order_id: [1; 16],
            },
            ExecLeg {
                venue: 2,
                instrument: VenueInstrumentRef::Kalshi("DRY-RUN-B".into()),
                limit_price: Prob::from_cents(49).unwrap(),
                stake_cents: 100,
                client_order_id: [2; 16],
            },
        ];
        let mut executor = HedgedExecutor { risk: &mut risk };
        let report = executor
            .execute(100, &legs, &DryRunAdapter, &DryRunAdapter)
            .expect("dry-run execution");
        println!(
            "dry-run session complete classification={:?} filled_stake_cents={}",
            report.classification, report.filled_stake_cents
        );
        tokio::time::sleep(std::time::Duration::from_millis(1)).await;
    }
}
