# Live trading integration acceptance gates

- [x] G1: Workspace builds with the live feed feature and execution crate.
  - CHECK: `cargo check --workspace --all-features`
  - EXPECT: `Finished`
  - EVIDENCE: cargo check --workspace --all-features passed (Finished)
- [x] G2: Workspace tests cover execution risk, hedging, catalog validation, and existing regressions.
  - CHECK: `cargo test --workspace --all-features`
  - EXPECT: `test result: ok`
  - EVIDENCE: cargo test --workspace --all-features passed; all test result lines were ok
- [x] G3: Formatting and linting pass for the implemented Rust surfaces.
  - CHECK: `cargo fmt --all --check && echo format-check-passed`
  - EXPECT: `format-check-passed`
  - EVIDENCE: cargo fmt --all --check and workspace clippy passed
- [x] G4: Dashboard accepts and renders live execution records without recalculating authoritative PnL.
  - CHECK: `npm --prefix dashboard test -- --run`
  - EXPECT: `Tests  passed`
  - EVIDENCE: dashboard Vitest 7 files / 50 tests passed
- [x] G5: Live mode cannot place orders when the kill switch is enabled or risk limits fail.
  - CHECK: `cargo test -p arbkit-exec --all-features`
  - EXPECT: `test result: ok`
  - EVIDENCE: arbkit-exec tests 2 passed; kill switch and partial-fill unwind covered

## HJ-58 Kalshi execution adapter

- [x] HJ58-G1: The execution crate builds with the live adapter enabled.
  - CHECK: `cargo check -p arbkit-exec --features live`
  - EXPECT: `Finished`
  - EVIDENCE: cargo check completed successfully.
- [x] HJ58-G2: Kalshi request signing covers method, path, timestamp, and RSA-PSS verification.
  - CHECK: `cargo test -p arbkit-exec --features live kalshi::tests::signing`
  - EXPECT: `test result: ok`
  - EVIDENCE: signing_round_trip, signing_rejects_missing_credentials, and request_body_uses_fok_and_idempotency_key passed.
- [x] HJ58-G3: Demo HTTP fixtures cover order placement, cancellation, status, balance, and authentication failures.
  - CHECK: `cargo test -p arbkit-exec --features live kalshi::tests::http_fixtures`
  - EXPECT: `test result: ok`
  - EVIDENCE: http_fixtures and http_rejects_authentication_failure passed with local socket permission enabled.
- [x] HJ58-G4: Formatting and all workspace regressions pass.
  - CHECK: `cargo fmt --all --check && cargo test --workspace --all-features`
  - EXPECT: `test result: ok`
  - EVIDENCE: formatting check and full all-features workspace suite passed; all test result lines were ok.

## HJ-59 through HJ-62 integration gates

- [x] HJ59-G1: Polymarket authenticated adapter supports L2 signing, FOK/IOC construction, cancellation, status, balance, and recorded HTTP fixtures.
  - CHECK: `cargo test -p arbkit-exec --all-features polymarket`
  - EXPECT: `test result: ok`
  - EVIDENCE: signature, policy, private fill, and HTTP fixture tests passed.
- [x] HJ60-G1: Risk state persists atomically and exposes restart, rate-limit, stale-feed, and emergency-flatten controls.
  - CHECK: `cargo test -p arbkit-exec --all-features state`
  - EXPECT: `test result: ok`
  - EVIDENCE: durable snapshot/reconciliation test passed; APIs compile under strict Clippy.
- [x] HJ61-G1: The dry-run trader executes a guarded Tokio session through the hedge executor.
  - CHECK: `cargo run -p arbkit-exec --example live_trader -- --mode=dry-run`
  - EXPECT: `dry-run session complete classification=LiveFill`
  - EVIDENCE: smoke run completed with a LiveFill report and the kill switch remained the default live-mode guard.
- [x] HJ62-G1: Private fill frames reconcile by client/venue IDs into authoritative fees and PnL.
  - CHECK: `cargo test -p arbkit-exec --all-features private_fill`
  - EXPECT: `test result: ok`
  - EVIDENCE: Kalshi and Polymarket private fill parser tests plus durable ledger round-trip passed.

## HJ-63 protected dashboard live ingest gates

- [x] HJ63-G1: Ingest is token-gated before any request reaches the Durable Object.
  - CHECK: `npm --prefix dashboard test -- --run worker/index.test.ts`
  - EXPECT: `Tests passed` across the credential matrix
  - EVIDENCE: correct bearer forwards to the room; missing/garbled/mis-schemed credentials all answer 401 with zero room calls; unconfigured token answers 503; method gate answers 405 first.
- [x] HJ63-G2: At-least-once ingest retries and resume replays are idempotent.
  - CHECK: `npm --prefix dashboard test -- --run worker/position-room.test.ts src/data/liveSession.test.ts`
  - EXPECT: `Tests passed`
  - EVIDENCE: a replayed whole batch leaves totals/ring unchanged server-side; the viewer handshake serves resume slices strictly above `afterSeq`; reducer drops rows at or below its cursor.
- [x] HJ63-G3: Session restart resets authoritative state and fans it out.
  - CHECK: `npm --prefix dashboard test -- --run worker/state.test.ts`
  - EXPECT: `Tests passed`
  - EVIDENCE: new session-start zeroes totals/cursor/ring and snapshots under the new runId; ended sessions reject further frames with 409 until a session-start.
- [x] HJ63-G4: Live vs paper labeling and open/unwound settlement rendering are honest.
  - CHECK: `npm --prefix dashboard test -- --run src/components/LivePoc.test.tsx`
  - EXPECT: `Tests passed`
  - EVIDENCE: paper sessions show the synthetic-workload note only; a live record raises "Live Trading: real capital, not synthetic"; open/unwound settlements render as status text instead of fabricated $0.00 and are not styled as losses.
- [x] HJ63-G5: Staleness flips exactly once via alarm and recovers on runner return; operational stats surface on every push.
  - CHECK: `npm --prefix dashboard test -- --run && npm --prefix dashboard run typecheck:worker && npm --prefix dashboard run build`
  - EXPECT: `Tests 9 files / 75 passed`; typecheck and build clean
  - EVIDENCE: alarm lifecycle (arm, keep-earlier, stale-once, die-out, re-arm on resume) plus stats-frame surfacing of cursor/windows/capital/funnel all pinned; full suite, worker typecheck, and production build pass.

## HJ-64 testnet integration and failure-drill gates

- [x] HJ64-G1: Kalshi demo and Polymarket sandbox presets exist as configuration without network claims.
  - CHECK: `cargo test -p arbkit-exec --all-features --test failure_drills demo_and_sandbox`
  - EXPECT: `test result: ok`
  - EVIDENCE: `KalshiConfig::demo` targets `demo-api.kalshi.co`, `PolymarketConfig::sandbox` targets the staging CLOB; constructing either performs no I/O.
- [x] HJ64-G2: The ten failure drills pass against scripted local venues over the real signed adapters.
  - CHECK: `cargo test -p arbkit-exec --all-features --test failure_drills`
  - EXPECT: `test result: ok. 11 passed`
  - EVIDENCE: full fill holds reserve; rejection unwinds and releases; partial fill is a phantom; failed unwind conservatively holds capital; timeout fails one leg and unwinds the other; duplicate retry settles once by client id; stale feed blocks then recovers on observe; restart restores gate/bankroll from disk and reconciles settlement by client id with idempotent acknowledgement; kill switch refuses with zero venue traffic; balance mismatch aborts before any order POST.
- [x] HJ64-G3: Checkpointing can no longer erase crash-recovery state.
  - CHECK: `cargo test -p arbkit-exec --all-features restart` (drill) plus `state` unit tests
  - EXPECT: `test result: ok`
  - EVIDENCE: `RiskStateStore::checkpoint` merges live risk fields over stored state while preserving `in_flight`; `ReconciliationLedger::apply_fill` now dedupes replayed fill events so at-least-once delivery cannot double-count fees or profit (unit-pinned).
- [x] HJ64-G4: Adapters fail fast instead of stalling the runner.
  - CHECK: `cargo test -p arbkit-exec --all-features timeout` drill
  - EXPECT: `test result: ok`
  - EVIDENCE: both adapters gained a configurable per-request deadline (`request_timeout`, default 5 s); a stalled venue degrades to a phantom hedge whose accepted leg is unwound.
- [x] HJ64-G5: Workspace regressions and formatting hold with all features enabled.
  - CHECK: `cargo fmt --all --check && cargo clippy --workspace --all-targets --all-features -- -D warnings && cargo test --workspace --all-features`
  - EXPECT: clean
  - EVIDENCE: fmt check passed; clippy clean across the workspace; every workspace test binary reported ok including 14 arbkit-exec unit tests and 11 drills; `live_trader` dry-run smoke still completes with LiveFill.

## HJ-65 same-tape proof and readiness review gates

- [x] HJ65-G1: Occurrence tapes replay through the paper simulator into a comparable proof report.
  - CHECK: `cargo test -p arbkit-exec --features paper-replay --test same_tape_proof`
  - EXPECT: `test result: ok. 5 passed`
  - EVIDENCE: parity tape (arrivals = quotes) reduces to 3 fills / 0 phantoms / 30c realized on 570c staked (526 bps floored); malformed tapes rejected with named errors.
- [x] HJ65-G2: Paper-vs-live comparison is integer-exact and falsification is machine-readable.
  - CHECK: `same_tape_proof` example with `--compare` against agreeing and divergent artifacts
  - EXPECT: exit 0 inside tolerance; exit 1 with `"within_tolerance":false` when falsified
  - EVIDENCE: fee-drag artifact compared at −18 bps → within 50 bps band, exit 0; fabricated-book artifact diverged −5,790 bps → falsified, exit 1. Negative ROI reported as a finding, never relabeled.
- [x] HJ65-G3: Broken-leg replay semantics are pinned so artifacts are read honestly.
  - CHECK: `cargo test -p arbkit-exec --features paper-replay --test same_tape_proof moved_price`
  - EXPECT: `test result: ok`
  - EVIDENCE: a repriced arrival on one leg fills the other leg alone — one phantom of the broken-leg kind carrying the full directional loss (−100c), not a clean partial fill; vanished quotes behave identically.
- [x] HJ65-G4: Dry-run warmup, micro-live acceptance criteria, and the pre-capital readiness checklist are documented.
  - CHECK: `LIVE_TRADING.md` § Same-tape proof procedure / Acceptance criteria / Production readiness review
  - EVIDENCE: procedure with exact commands and exit-code contract; warmup requires zero unwind failures plus green drills plus reconciled in-flight state; micro-live caps stakes/budget and mandates per-session comparisons; checklist covers credentials, kill-switch posture, catalog gate, persisted risk limits, timeouts, runbook coverage, and dated falsification records.
- [x] HJ65-G5: Workspace regressions hold with the new feature enabled and disabled.
  - CHECK: `cargo fmt --all --check && cargo clippy --workspace --all-targets --all-features -- -D warnings && cargo test --workspace --all-features`
  - EXPECT: clean
  - EVIDENCE: all 22 workspace test binaries ok (including the new `same_tape_proof` suite); `cargo check -p arbkit-exec` with default features confirms the replay surface stays out of default builds; RESULTS.md §8 records this host's dated proof-harness measurements.
