//! Same-tape proof: replay an occurrence tape through the paper simulator
//! and compare the result against a live session's proof artifact.
//!
//! Usage:
//! ```text
//! cargo run -p arbkit-exec --features paper-replay --example same_tape_proof -- \
//!     --input occurrences.ndjson [--compare live-proof.json] [--tolerance-bps 50]
//! ```
//!
//! Exit status is `0` while paper and live agree inside tolerance and `1`
//! when they do not — a falsified synthetic assumption is a *result*, not an
//! error to swallow, but automation needs a machine-readable verdict.

use std::path::PathBuf;

use arbkit_exec::{compare_tape, replay_paper_tape, LiveProofReport, OccurrenceRecord};

struct Args {
    input: PathBuf,
    compare: Option<PathBuf>,
    tolerance_bps: i64,
}

fn parse_args() -> Result<Args, String> {
    let mut input = None;
    let mut compare = None;
    let mut tolerance_bps = 50i64;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--input" => input = Some(PathBuf::from(args.next().ok_or("--input requires a path")?)),
            "--compare" => {
                compare = Some(PathBuf::from(
                    args.next().ok_or("--compare requires a path")?,
                ))
            }
            "--tolerance-bps" => {
                tolerance_bps = args
                    .next()
                    .ok_or("--tolerance-bps requires a number")?
                    .parse()
                    .map_err(|e| format!("--tolerance-bps: {e}"))?;
            }
            other => return Err(format!("unknown argument {other:?}")),
        }
    }
    Ok(Args {
        input: input.ok_or("--input <occurrences.ndjson> is required")?,
        compare,
        tolerance_bps,
    })
}

fn read_records(path: &std::path::Path) -> Result<Vec<OccurrenceRecord>, String> {
    let raw = std::fs::read_to_string(path).map_err(|e| format!("read {}: {e}", path.display()))?;
    let mut records = Vec::new();
    for (index, line) in raw.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        records.push(
            serde_json::from_str(line)
                .map_err(|e| format!("{} line {}: {e}", path.display(), index + 1))?,
        );
    }
    Ok(records)
}

#[tokio::main]
async fn main() {
    let args = parse_args().unwrap_or_else(|e| {
        eprintln!("{e}");
        std::process::exit(2);
    });

    let records = read_records(&args.input).unwrap_or_else(|e| {
        eprintln!("{e}");
        std::process::exit(2);
    });
    let paper = replay_paper_tape(&records).unwrap_or_else(|e| {
        eprintln!("{e}");
        std::process::exit(2);
    });

    if let Some(live_path) = &args.compare {
        let live_raw = std::fs::read_to_string(live_path).unwrap_or_else(|e| {
            eprintln!("read {}: {e}", live_path.display());
            std::process::exit(2);
        });
        let live: LiveProofReport = serde_json::from_str(&live_raw).unwrap_or_else(|e| {
            eprintln!(
                "{} is not a LiveProofReport artifact: {e}",
                live_path.display()
            );
            std::process::exit(2);
        });
        let comparison = compare_tape(&paper, &live, args.tolerance_bps);
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "paper": paper,
                "live": live,
                "comparison": comparison,
                "tolerance_bps": args.tolerance_bps,
                "occurrences": records.len(),
                "falsified": !comparison.within_tolerance,
            }))
            .expect("serialize comparison")
        );
        if !comparison.within_tolerance {
            // A divergence beyond tolerance falsifies the assumption that
            // paper results transfer; report it, never hide it.
            eprintln!(
                "same-tape check FAILED: live ROI diverges from paper by {} bps",
                comparison.roi_delta_bps
            );
            std::process::exit(1);
        }

        // Phantom-rate halt: live phantoms running more than 10 percentage
        // points (1000 bps) above the paper baseline means the fill model no
        // longer describes the venue — halt micro-live and re-arm.
        let rate = |report: &LiveProofReport| -> i64 {
            if report.attempted_arbs == 0 {
                return 0;
            }
            (report.live_phantoms as i64 * 10_000).div_euclid(report.attempted_arbs as i64)
        };
        let (paper_rate, live_rate) = (rate(&paper), rate(&live));
        if live_rate - paper_rate > 1_000 {
            eprintln!(
                "PHANTOM-RATE HALT: live {} bps vs paper {} bps (>{:?} delta) — \
                 re-arm the kill switch and explain before resuming",
                live_rate, paper_rate, 1_000i64
            );
            std::process::exit(2);
        }
    } else {
        println!("{}", paper.to_json().expect("serialize paper report"));
    }
}
