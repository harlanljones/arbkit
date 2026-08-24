//! Minimal live-trader entry point.
//!
//! The binary intentionally starts in dry-run/kill-switch mode. Feed and
//! catalog wiring can be supplied by the host application, while this command
//! validates the execution policy before any adapter is constructed.

use std::env;

use arbkit_exec::{ExecMode, RiskConfig};

fn main() {
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
    println!(
        "execution policy validated; no feed or order adapter is active in this standalone command"
    );
}
