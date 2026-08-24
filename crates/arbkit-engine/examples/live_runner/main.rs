//! Live paper-trading position streamer.
//!
//! Runs the same engine + simulator stack as `pipeline.rs`, restructured into
//! fixed wall-clock windows so detected-and-settled positions arrive at the
//! dashboard continuously instead of in one burst at exit. Each window pushes
//! its slice of the scripted synthetic feed through the hot loop, drains the
//! signal ring, sizes and settles every signal against the bankroll exactly
//! as the batch pipeline would, then hands finished [`TradeRecord`]s to the
//! writer thread (`stream`) for delivery.
//!
//! Everything here sits downstream of the SPSC ring — post-consumption work
//! only. The hot path never learns that a network exists.
//!
//! Usage:
//! ```text
//! cargo run --example live_runner -- \
//!     --url http://127.0.0.1:8787/api/live/ingest \
//!     [--token-env VAR] [--ticks-per-window 200] [--window-ms 1000]
//!     [--bankroll <cents>] [--windows <n>]
//! ```
//!
//! Without `--windows` the runner streams until killed; the session is
//! declared stale by heartbeat timeout, never by a graceful goodbye.

mod frames;
mod stream;

// The shared ledger module is included whole, so its file-writing surface
// (used by the pipeline example) is unused here — that is inclusion, not
// dead design.
#[path = "../trades_ledger/mod.rs"]
#[allow(dead_code)]
mod trades_ledger;

use std::env;
use std::fs;
use std::path::PathBuf;
use std::process;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use arbkit_core::{Fee, Level, MarketKind, Prob};
use arbkit_engine::{spsc_ring, Engine, FeedEventSlot, MarketConfig, SignalEvent, SignalEventSlot};
use arbkit_feed::{FeedEvent, TradeSide};
use arbkit_match::team::{parse_matchup, Sport};
use arbkit_match::{CanonicalRegistry, VenueRegistry};
use arbkit_sim::{Bankroll, LatencyModel, LatencyProfile, Simulator};

use frames::{LiveFrame, LIVE_SCHEMA_VERSION};
use stream::{StreamConfig, StreamHandle};
use trades_ledger::{build_trade_record, LabelResolver, TradeRecord};

/// Resolves interned engine ids to human-readable labels through the match
/// registries built at startup. Same contract as the pipeline's resolver:
/// lookup misses degrade to `"market:<id>"`-style strings, never panic.
struct LiveLabels<'a> {
    registry: &'a CanonicalRegistry,
    venues: &'a VenueRegistry,
}

impl LabelResolver for LiveLabels<'_> {
    fn market_label(&self, market_id: u32) -> String {
        let Some(market) = self.registry.get_market(market_id) else {
            return format!("market:{market_id}");
        };
        let kind = match market.kind {
            MarketKind::Moneyline => "moneyline".to_string(),
            MarketKind::Spread(line) => format!("spread {line:?}"),
            MarketKind::Total(line) => format!("total {line:?}"),
        };
        match self.registry.get_event(market.event_id) {
            Some(event) => format!("{} · {}", event.name, kind),
            None => kind,
        }
    }

    fn venue_label(&self, venue_id: u16) -> String {
        self.venues
            .name_of(venue_id)
            .map(str::to_string)
            .unwrap_or_else(|| format!("venue:{venue_id}"))
    }

    fn outcome_label(&self, outcome_id: u32) -> String {
        self.registry
            .get_outcome(outcome_id)
            .map(|outcome| outcome.name.clone())
            .unwrap_or_else(|| format!("outcome:{outcome_id}"))
    }
}

/// Per-(venue, outcome) feed sequence counters. The script alternates venue
/// then outcome exactly as the pipeline's generator does, so a lost sequence
/// number means the same thing in both workloads.
#[derive(Debug, Default)]
struct SeqCounters {
    kalshi_bos: u64,
    kalshi_lal: u64,
    poly_bos: u64,
    poly_lal: u64,
}

impl SeqCounters {
    fn bump(&mut self, is_kalshi: bool, is_bos: bool) -> u64 {
        match (is_kalshi, is_bos) {
            (true, true) => {
                self.kalshi_bos += 1;
                self.kalshi_bos
            }
            (true, false) => {
                self.kalshi_lal += 1;
                self.kalshi_lal
            }
            (false, true) => {
                self.poly_bos += 1;
                self.poly_bos
            }
            (false, false) => {
                self.poly_lal += 1;
                self.poly_lal
            }
        }
    }
}

/// The scripted synthetic workload, sliced one window at a time.
///
/// The price table is the pipeline's verbatim: a lucrative arbitrage window
/// opens for 21 of every 200 ticks, a marginal fee-eaten window follows, and
/// the rest is normal vig — so a streamed session reproduces a recorded run's
/// economics on whatever cadence the operator chooses.
struct SyntheticFeed {
    market_id: u32,
    outcome_bos: u32,
    outcome_lal: u32,
    start: Instant,
}

impl SyntheticFeed {
    fn clock_ns(&self) -> u64 {
        self.start.elapsed().as_nanos() as u64
    }

    /// Initial book snapshots for all four venue×outcome books, mirroring the
    /// pipeline's opening state (49/53 Kalshi vs 51/49 Polymarket).
    fn initial_snapshots(&self, seqs: &mut SeqCounters) -> Vec<FeedEvent> {
        [
            (VenueRegistry::KALSHI, self.outcome_bos, 49),
            (VenueRegistry::KALSHI, self.outcome_lal, 53),
            (VenueRegistry::POLYMARKET, self.outcome_bos, 51),
            (VenueRegistry::POLYMARKET, self.outcome_lal, 49),
        ]
        .into_iter()
        .map(|(venue_id, outcome_id, cents)| {
            let seq = seqs.bump(
                venue_id == VenueRegistry::KALSHI,
                outcome_id == self.outcome_bos,
            );
            self.snapshot(venue_id, outcome_id, cents, 100_000, seq)
        })
        .collect()
    }

    fn snapshot(
        &self,
        venue_id: u16,
        outcome_id: u32,
        price_cents: u32,
        size: i64,
        seq: u64,
    ) -> FeedEvent {
        const PAD: [Level; 6] = [
            Level {
                price: Prob::CERTAIN,
                size: 0,
            },
            Level {
                price: Prob::CERTAIN,
                size: 0,
            },
            Level {
                price: Prob::CERTAIN,
                size: 0,
            },
            Level {
                price: Prob::CERTAIN,
                size: 0,
            },
            Level {
                price: Prob::CERTAIN,
                size: 0,
            },
            Level {
                price: Prob::CERTAIN,
                size: 0,
            },
        ];
        FeedEvent::Snapshot {
            market_id: self.market_id,
            outcome_id,
            venue_id,
            levels: [
                Level {
                    price: Prob::from_cents(price_cents).expect("script prices are valid cents"),
                    size,
                },
                Level {
                    price: Prob::from_cents(price_cents.saturating_add(1).min(99))
                        .expect("script prices are valid cents"),
                    size: size * 2,
                },
                PAD[0],
                PAD[1],
                PAD[2],
                PAD[3],
                PAD[4],
                PAD[5],
            ],
            num_levels: 2,
            seq,
            timestamp_ns: self.clock_ns(),
        }
    }

    /// The scripted price for `tick`, in cents. Identical table to the
    /// pipeline's generator so streamed sessions reproduce its economics.
    fn scripted_price(&self, tick: usize, venue_id: u16, outcome_id: u32) -> u32 {
        let is_bos = outcome_id == self.outcome_bos;
        match tick % 200 {
            // Lucrative arbitrage window (~440 bp edge).
            0..=20 => {
                if is_bos {
                    if venue_id == VenueRegistry::KALSHI {
                        46
                    } else {
                        53
                    }
                } else if venue_id == VenueRegistry::POLYMARKET {
                    48
                } else {
                    55
                }
            }
            // Marginal window: fees eat the edge.
            21..=35 => {
                if is_bos {
                    if venue_id == VenueRegistry::KALSHI {
                        48
                    } else {
                        52
                    }
                } else if venue_id == VenueRegistry::POLYMARKET {
                    50
                } else {
                    52
                }
            }
            // Normal vig market.
            _ => {
                let shift = (tick % 4) as u32;
                if is_bos {
                    50 + shift
                } else {
                    52 + shift
                }
            }
        }
    }

    /// Builds the feed event for global tick `tick`, rotating sequence
    /// counters exactly as the pipeline does (alternating venue, alternating
    /// outcome).
    fn next_event(&self, tick: usize, seqs: &mut SeqCounters) -> FeedEvent {
        let is_kalshi = (tick % 2) == 0;
        let is_bos = ((tick / 2) % 2) == 0;
        let venue_id = if is_kalshi {
            VenueRegistry::KALSHI
        } else {
            VenueRegistry::POLYMARKET
        };
        let outcome_id = if is_bos {
            self.outcome_bos
        } else {
            self.outcome_lal
        };
        let seq = seqs.bump(is_kalshi, is_bos);

        let price_cents = self.scripted_price(tick, venue_id, outcome_id);

        // Every 5000th event is a trade print; the rest are book deltas with
        // realistic depth jitter, matching the pipeline workload.
        if tick % 5000 == 4999 {
            FeedEvent::Trade {
                market_id: self.market_id,
                outcome_id,
                venue_id,
                price: Prob::from_cents(price_cents.min(99)).expect("script prices are valid"),
                size: 20_000,
                side: TradeSide::Buy,
                seq,
                timestamp_ns: self.clock_ns(),
            }
        } else {
            FeedEvent::Delta {
                market_id: self.market_id,
                outcome_id,
                venue_id,
                level: Level {
                    price: Prob::from_cents(price_cents.min(99)).expect("script prices are valid"),
                    size: 50_000 + ((tick % 5) as i64) * 20_000,
                },
                is_delete: false,
                seq,
                timestamp_ns: self.clock_ns(),
            }
        }
    }
}

struct RunnerArgs {
    url: String,
    token: String,
    ticks_per_window: usize,
    window_ms: u64,
    bankroll_cents: i64,
    windows: Option<usize>,
}

const USAGE: &str = "Usage: live_runner [--url <ingest-url>] [--token-env <VAR>] \
[--ticks-per-window <n>] [--window-ms <ms>] [--bankroll <cents>] [--windows <n>]";

fn parse_args() -> RunnerArgs {
    let mut args = RunnerArgs {
        url: String::from("http://127.0.0.1:8787/api/live/ingest"),
        // Default token mirrors the dashboard's own resolution order so one
        // `.dev.vars` serves both sides of the stream: `--token-env` names an
        // explicit override, the environment wins over the file, and the file
        // is found whether the runner starts at the repo root or in
        // `dashboard/`.
        token: resolve_default_token(),
        ticks_per_window: 200,
        window_ms: 1_000,
        bankroll_cents: 0,
        windows: None,
    };

    let mut argv = env::args().skip(1);
    while let Some(flag) = argv.next() {
        let mut value = |name: &str| -> String {
            argv.next().unwrap_or_else(|| {
                eprintln!("error: {name} requires a value\n{USAGE}");
                process::exit(2);
            })
        };
        match flag.as_str() {
            "--url" => args.url = value("--url"),
            "--token-env" => {
                let var = value("--token-env");
                args.token = env::var(&var).unwrap_or_default();
                if args.token.is_empty() {
                    eprintln!("warning: ${var} is unset; ingest goes out unauthenticated");
                }
            }
            "--ticks-per-window" => {
                args.ticks_per_window = value("--ticks-per-window").parse().unwrap_or_else(|_| {
                    eprintln!("error: --ticks-per-window must be a positive count");
                    process::exit(2);
                });
            }
            "--window-ms" => {
                args.window_ms = value("--window-ms").parse().unwrap_or_else(|_| {
                    eprintln!("error: --window-ms must be milliseconds");
                    process::exit(2);
                });
            }
            "--bankroll" => {
                args.bankroll_cents = value("--bankroll").parse().unwrap_or_else(|_| {
                    eprintln!("error: --bankroll must be a cents amount");
                    process::exit(2);
                });
                if args.bankroll_cents < 0 {
                    eprintln!("error: --bankroll must be non-negative");
                    process::exit(2);
                }
            }
            "--windows" => {
                args.windows = Some(value("--windows").parse().unwrap_or_else(|_| {
                    eprintln!("error: --windows must be a positive count");
                    process::exit(2);
                }));
            }
            other => {
                eprintln!("error: unknown flag {other}\n{USAGE}");
                process::exit(2);
            }
        }
    }
    if args.token.is_empty() && !args.url.starts_with("http://127.0.0.1") {
        eprintln!(
            "warning: no token found (env, --token-env, or dashboard/.dev.vars); \
             the ingest will be rejected"
        );
    }
    args
}

/// Token resolution order: the `--token-env` variable, then the environment,
/// then `dashboard/.dev.vars` beside or above the working directory — the
/// same file `wrangler dev` reads, so one source of truth serves both sides
/// of the stream and a bare `cargo run` from either directory just works.
fn resolve_default_token() -> String {
    if let Ok(token) = env::var("LIVE_INGEST_TOKEN") {
        return token;
    }
    if let Ok(token) = env::var("ARBLIVE_TOKEN") {
        return token;
    }
    read_dot_dev_vars_token()
}

/// Scans candidate `.dev.vars` locations for `LIVE_INGEST_TOKEN`. The file
/// never overrides an exported variable — dotenv semantics, deliberately.
fn read_dot_dev_vars_token() -> String {
    for candidate in [
        PathBuf::from("dashboard/.dev.vars"),
        PathBuf::from(".dev.vars"),
    ] {
        let Ok(contents) = fs::read_to_string(&candidate) else {
            continue;
        };
        for line in contents.lines() {
            if let Some(value) = line.strip_prefix("LIVE_INGEST_TOKEN=") {
                let trimmed = value.trim();
                if !trimmed.is_empty() {
                    return trimmed.to_string();
                }
            }
        }
    }
    String::new()
}

fn git_commit_short() -> Option<String> {
    let output = process::Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .output()
        .ok()?;
    let trimmed = String::from_utf8(output.stdout).ok()?.trim().to_string();
    (!trimmed.is_empty()).then_some(trimmed)
}

fn main() {
    let args = parse_args();

    println!("========================================================================");
    println!("  arbkit: Live Paper-Trading Position Stream");
    println!("========================================================================");

    // 1. Canonical registry — same fixture as the pipeline.
    let mut registry = CanonicalRegistry::new();
    let matchup = parse_matchup("BOS @ LAL", Some(Sport::Nba)).expect("parse matchup");
    let event_id = registry.create_event(
        "Boston Celtics @ Los Angeles Lakers",
        Sport::Nba,
        matchup.home,
        matchup.away,
        Some("26OCT25"),
    );
    let (moneyline_market, outcome_lal, outcome_bos) = registry
        .create_moneyline_market(event_id)
        .expect("create moneyline market");

    // 2. Engine and market configuration — same venues, fees, increments and
    // transit-survival discounts as the pipeline, so streamed numbers are the
    // numbers a recorded run would have produced.
    let mut engine = Engine::new(16);
    let mut config = MarketConfig {
        outcome_count: 2,
        active: true,
        ..Default::default()
    };
    config.venue_fees[VenueRegistry::KALSHI as usize] = Fee::StakeFeeBps(350);
    config.venue_increments[VenueRegistry::KALSHI as usize] = 100;
    config.venue_fees[VenueRegistry::POLYMARKET as usize] = Fee::None;
    config.venue_increments[VenueRegistry::POLYMARKET as usize] = 1;
    config.venue_fees[VenueRegistry::PINNACLE as usize] = Fee::StakeFeeBps(100);
    config.venue_increments[VenueRegistry::PINNACLE as usize] = 100;
    config.venue_survival_bps[VenueRegistry::KALSHI as usize] = 10_000 - 500;
    config.venue_survival_bps[VenueRegistry::POLYMARKET as usize] = 10_000 - 1_000;
    config.venue_survival_bps[VenueRegistry::PINNACLE as usize] = 10_000 - 500;
    engine
        .register_market(moneyline_market, config)
        .expect("register market");

    // 3. Rings and the dedicated hot-loop thread.
    const RING_CAPACITY: usize = 8192;
    let (mut feed_prod, mut feed_cons) = spsc_ring::<FeedEventSlot>(RING_CAPACITY);
    let (mut signal_prod, mut signal_cons) = spsc_ring::<SignalEventSlot>(RING_CAPACITY);

    let engine_running = Arc::new(AtomicBool::new(true));
    let engine_running_flag = engine_running.clone();
    let engine_start = Instant::now();
    let engine_thread = thread::Builder::new()
        .name("hot-engine-loop".into())
        .spawn(move || {
            while engine_running_flag.load(Ordering::Relaxed) {
                if let Some(event) = feed_cons.try_pop() {
                    let now_ns = engine_start.elapsed().as_nanos() as u64;
                    if let Some(signal_event) = engine.process_event(&event, now_ns) {
                        let _ = signal_prod.try_push(signal_event);
                    }
                }
            }
            while let Some(event) = feed_cons.try_pop() {
                let now_ns = engine_start.elapsed().as_nanos() as u64;
                if let Some(signal_event) = engine.process_event(&event, now_ns) {
                    let _ = signal_prod.try_push(signal_event);
                }
            }
        })
        .expect("spawn engine thread");

    // 4. Simulator latency model — identical to the pipeline's profiles.
    let mut latency_model = LatencyModel::new(LatencyProfile {
        wire_delay_ns: 10_000_000,
        venue_processing_ns: 2_000_000,
        queue_front_run_bps: 500,
    });
    latency_model.set_venue_profile(
        VenueRegistry::KALSHI,
        LatencyProfile {
            wire_delay_ns: 8_000_000,
            venue_processing_ns: 2_000_000,
            queue_front_run_bps: 500,
        },
    );
    latency_model.set_venue_profile(
        VenueRegistry::POLYMARKET,
        LatencyProfile {
            wire_delay_ns: 12_000_000,
            venue_processing_ns: 3_000_000,
            queue_front_run_bps: 1_000,
        },
    );
    let mut simulator = Simulator::new(latency_model);

    let mut bankroll = if args.bankroll_cents > 0 {
        let per_venue = args.bankroll_cents / 3;
        Some(
            Bankroll::new(&[per_venue, per_venue, args.bankroll_cents - 2 * per_venue])
                .expect("bankroll amounts are validated non-negative at the CLI"),
        )
    } else {
        None
    };

    // 5. Streaming writer and session opener.
    let stream = StreamHandle::spawn(StreamConfig {
        url: args.url.clone(),
        token: args.token.clone(),
        ..Default::default()
    });

    let started_at_epoch_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock is before Unix epoch")
        .as_millis();
    let run_id = format!(
        "{}-{}-{}-{}-live",
        started_at_epoch_ms,
        env::consts::OS,
        env::consts::ARCH,
        git_commit_short().as_deref().unwrap_or("working-tree")
    );

    stream.send(LiveFrame::SessionStart {
        schema_version: LIVE_SCHEMA_VERSION,
        run_id: run_id.clone(),
        started_at_epoch_ms,
        initial_bankroll_cents: bankroll.as_ref().map(Bankroll::total_available),
        ticks_per_window: args.ticks_per_window,
        window_ms: args.window_ms,
    });
    println!("Streaming live session {run_id}");
    println!("  ingest: {}", args.url);
    println!(
        "  windows: {} ticks @ {} ms{}",
        args.ticks_per_window,
        args.window_ms,
        match args.windows {
            Some(limit) => format!(", stopping after {limit}"),
            None => String::from(", until killed"),
        }
    );

    // 6. Opening book state, then the windowed streaming loop.
    let feed = SyntheticFeed {
        market_id: moneyline_market,
        outcome_bos,
        outcome_lal,
        start: Instant::now(),
    };
    let mut seqs = SeqCounters::default();
    for snapshot in feed.initial_snapshots(&mut seqs) {
        while feed_prod.try_push(snapshot).is_err() {
            thread::yield_now();
        }
    }

    let venue_registry = VenueRegistry::new();
    let labels = LiveLabels {
        registry: &registry,
        venues: &venue_registry,
    };
    let mut collected: Vec<SignalEvent> = Vec::with_capacity(256);
    let mut seen_signals = 0usize;
    let mut seq_cursor = 0u64;
    let mut windows_completed = 0usize;
    let window_duration = Duration::from_millis(args.window_ms);

    let mut window_index = 0usize;
    loop {
        if let Some(limit) = args.windows {
            if window_index >= limit {
                break;
            }
        }
        let window_started = Instant::now();

        for k in 0..args.ticks_per_window {
            let event = feed.next_event(window_index * args.ticks_per_window + k, &mut seqs);
            while feed_prod.try_push(event).is_err() {
                if let Some(signal_event) = signal_cons.try_pop() {
                    collected.push(signal_event);
                }
                std::hint::spin_loop();
            }
            if k % 16 == 0 {
                while let Some(signal_event) = signal_cons.try_pop() {
                    collected.push(signal_event);
                }
            }
        }

        // Let the hot loop finish the window's backlog, then collect.
        thread::sleep(Duration::from_millis(20));
        while let Some(signal_event) = signal_cons.try_pop() {
            collected.push(signal_event);
        }

        // Size and settle each collected signal exactly as the batch
        // pipeline does: full-reserve gate first, worst-payout settlement
        // after, so a streamed trade's numbers equal a recorded trade's.
        let mut items: Vec<TradeRecord> = Vec::with_capacity(collected.len());
        for signal_event in collected.drain(..) {
            seen_signals += 1;
            let plan_len = signal_event.plan_len as usize;
            let legs = &signal_event.plan[..plan_len];
            let allocs = signal_event.signal.allocations();
            if plan_len < 2 || allocs.len() != plan_len {
                continue; // defensive: malformed plan is not tradeable
            }

            let arrival_prices: Vec<Option<Prob>> = legs
                .iter()
                .map(|leg| {
                    if seen_signals % 10 == 1 {
                        // Scripted phantom scenario (synthetic workload only):
                        // every leg decays 3¢ in flight, labeled by the
                        // record's classification once settled.
                        Some(
                            Prob::from_ppm((leg.quoted.ppm() + 30_000).min(990_000))
                                .expect("decayed quote is valid ppm"),
                        )
                    } else {
                        // Live resting prices match the detected quote.
                        Some(leg.quoted)
                    }
                })
                .collect();
            let arrival_depths: Vec<i64> = legs.iter().map(|leg| leg.capacity).collect();

            // Capital gate: reserve each leg's requested stake before sending.
            // A venue coming up short skips the whole trade — a partially
            // reserved hedge is unhedged directional risk, not half an arb.
            if let Some(bankroll) = bankroll.as_mut() {
                let mut reserved: Vec<(u16, i64)> = Vec::with_capacity(plan_len);
                let mut affordable = true;
                for (leg, alloc) in legs.iter().zip(allocs.iter()) {
                    if !bankroll.reserve(leg.venue, alloc.stake) {
                        affordable = false;
                        break;
                    }
                    reserved.push((leg.venue, alloc.stake));
                }
                if !affordable {
                    for (venue, stake) in &reserved {
                        bankroll.commit_fill(*venue, 0, *stake);
                    }
                    simulator.record_capital_short(signal_event.signal.total_stake);
                    continue;
                }
            }

            let Ok(report) = simulator.simulate_with_quotes(
                signal_event.ingest_timestamp_ns,
                &signal_event.signal,
                legs,
                &arrival_prices,
                &arrival_depths,
            ) else {
                // Release any reservation before skipping: a failed simulation
                // must not leak locked capital.
                if let Some(bankroll) = bankroll.as_mut() {
                    for (leg, alloc) in legs.iter().zip(allocs.iter()) {
                        bankroll.commit_fill(leg.venue, 0, alloc.stake);
                    }
                }
                continue;
            };

            // Reconcile fills pessimistically: commit what filled, release
            // what did not, then assume the worst-payout side wins.
            if let Some(bankroll) = bankroll.as_mut() {
                for res in report.leg_results() {
                    bankroll.commit_fill(
                        res.venue,
                        res.filled_stake,
                        res.requested_stake - res.filled_stake,
                    );
                }
                let filled: Vec<_> = report
                    .leg_results()
                    .iter()
                    .filter(|res| res.filled_stake > 0)
                    .collect();
                if let Some((winner_index, winner)) = filled
                    .iter()
                    .enumerate()
                    .min_by_key(|(_, res)| res.net_payout)
                {
                    bankroll.settle_win(winner.venue, winner.filled_stake, winner.net_payout);
                    for (i, res) in filled.iter().enumerate() {
                        if i != winner_index {
                            bankroll.settle_loss(res.venue, res.filled_stake);
                        }
                    }
                }
            }

            items.push(build_trade_record(
                seq_cursor,
                &signal_event,
                &report,
                &labels,
            ));
            seq_cursor += 1;
        }

        stream.cursor().store(seq_cursor, Ordering::Relaxed);

        let realized_window: i64 = items.iter().map(|r| r.realized_profit_cents).sum();
        if !items.is_empty() {
            stream.send(LiveFrame::Positions { items });
        }
        let stats = simulator.stats();
        stream.send(LiveFrame::Stats {
            seq_cursor,
            windows_completed: window_index + 1,
            locked_cents: bankroll.as_ref().map(Bankroll::total_locked),
            available_cents: bankroll.as_ref().map(Bankroll::total_available),
            attempted: stats.attempted,
            capital_short: stats.capital_short,
        });
        windows_completed = window_index + 1;

        if realized_window != 0 {
            println!("[w{window_index:04}] settled trades · realized {realized_window:+}¢");
        } else if !collected.is_empty() {
            println!("[w{window_index:04}] signals processed · no net change");
        }

        window_index += 1;
        let elapsed = window_started.elapsed();
        if elapsed < window_duration {
            thread::sleep(window_duration - elapsed);
        }
    }

    // 7. Finite-run shutdown: best-effort goodbye, honest console totals.
    stream.send(LiveFrame::SessionEnd);
    let summary = stream.stop(true);

    engine_running.store(false, Ordering::Relaxed);
    engine_thread.join().expect("engine thread joins");

    let stats = simulator.stats();
    let realized_total = stats.total_realized_profit_cents;
    let staked_total = stats.total_filled_stake_cents;
    // ROI floored toward negative infinity via div_euclid: a reported ratio
    // rounds down even when negative — "one you can beat" cuts both ways.
    let realized_roi_bps = if staked_total > 0 {
        (realized_total * 10_000).div_euclid(staked_total)
    } else {
        0
    };

    println!();
    println!("========================================================================");
    println!("  Session Summary ({run_id})");
    println!("========================================================================");
    println!("  Windows Completed:         {:>12}", windows_completed);
    println!("  Trades Streamed:           {:>12}", seq_cursor);
    println!(
        "  Attempted / Capital-Short: {:>6} / {}",
        stats.attempted, stats.capital_short
    );
    println!(
        "  Clean / Proportional:      {:>6} / {}",
        stats.clean_fills, stats.proportional_fills
    );
    println!(
        "  Phantoms / Broken Legs:    {:>6} / {}",
        stats.total_phantoms, stats.broken_legs
    );
    println!("  Realized PnL:              {:>+12}¢", realized_total);
    println!("  Realized ROI:              {:>+9} bps", realized_roi_bps);
    println!(
        "  Stream Delivery:           {:>6} frames sent, {} batches dropped",
        summary.frames_sent, summary.batches_dropped
    );
    match bankroll.as_ref() {
        Some(bankroll) => println!(
            "  Bankroll Locked/Available: {}¢ / {}¢",
            bankroll.total_locked(),
            bankroll.total_available()
        ),
        None => println!("  Bankroll:                  static per-trade budget"),
    }
}
