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

## HJ-147 settlement reconciliation gates

- [x] HJ147-G1: The reconciler is I/O-free and unit-testable with a scripted source.
  - CHECK: `cargo test -p arbkit-exec reconcile`
  - EXPECT: `test result: ok`
  - EVIDENCE: `SettlementSource` carries venue access; `Reconciler` folds fills into the ledger and closes orders on terminal status. 5 reconciler tests pass (settle-once, dedupe, duplicate-fingerprint rejection, pending orders, restart-orphan clearing).
- [x] HJ147-G2: At-least-once delivery cannot double-count fees or profit.
  - CHECK: `idempotent_replays_do_not_double_count` + `rejects_a_duplicate_fill_fingerprint`.
  - EXPECT: realized profit and fees unchanged after a replay.
  - EVIDENCE: both tests pass; the ledger's `applied_fills` fingerprint set drops replays before any money is recomputed.
- [x] HJ147-G3: The runner registers in-flight, acknowledges venue ids, polls, and settles exactly once.
  - CHECK: code review of `examples/prod_trader.rs` + `HedgedExecutor::execute_reconciled`.
  - EXPECT: legs registered before submission; acknowledged per client id; `Reconciler::reconcile` polled every window; `risk.settle` called once per terminal order; `fills` frames emitted for both acknowledgement and settlement.
  - EVIDENCE: `execute_reconciled` returns `(report, Vec<ReconciledLeg>)` keyed by client id; the wire `ExecutionReport` is unchanged; reconcile step emits `Fills` frames and applies `risk.settle`; runner fixture smoke reaches session end with exit 0.
- [x] HJ147-G4: Restart reconciles orphaned in-flight entries by venue order id.
  - CHECK: `seeding_in_flight_clears_a_restart_orphan`.
  - EXPECT: a seeded in-flight order polled to `unwound` is cleared and its loss applied.
  - EVIDENCE: test passes; `Reconciler::seed_in_flight` re-registers orders into the ledger so `apply_fill` can find them.
- [x] HJ147-G5: Workspace hygiene holds with all features.
  - CHECK: `cargo fmt --all --check && cargo clippy --workspace --all-targets --all-features && cargo test --workspace --all-features`
  - EXPECT: clean.
  - EVIDENCE: fmt ok; clippy zero warnings; all 22 workspace test binaries ok.

## HJ-145 concurrent submission gates

- [x] HJ145-G1: Both legs are submitted concurrently, not one after the other.
  - CHECK: `cargo test -p arbkit-exec submits_both_legs_concurrently`
  - EXPECT: `test result: ok`
  - EVIDENCE: a 2-party-barrier adapter's `submit` blocks until both legs have entered, so a sequential two-call executor would hang rather than pass; the test passes and the hedge fills clean (200c).
- [x] HJ145-G2: The trait seam stays synchronous and runtime-free.
  - CHECK: `HedgedExecutor::execute` signature + code review.
  - EXPECT: `submit`/`unwind` remain blocking trait methods; concurrency uses `std::thread::scope` over borrowed adapters and blocks on join.
  - EVIDENCE: no `async`, no `tokio` in `execute`; unit tests run under `cargo test` with no runtime; the runner calls it from its own execution task.
- [x] HJ145-G3: Panicking an adapter degrades to a reject, never an abort.
  - CHECK: `join_submit` + `panic_message` behavior.
  - EXPECT: a scoped thread panic maps to a venue error string and the hedge is unwound like any other rejection.
  - EVIDENCE: helper collapses `ScopedJoinHandle` panics into `Err(String)` before the partial-fill unwind path.
- [x] HJ145-G4: Workspace hygiene holds with all features.
  - CHECK: `cargo fmt --all --check && cargo clippy --workspace --all-targets --all-features && cargo test --workspace --all-features`
  - EXPECT: clean.
  - EVIDENCE: fmt ok; clippy zero warnings; all 22 workspace test binaries ok.

## HJ-144 production runner gates

- [x] HJ144-G1: The runner builds behind its own feature without touching default targets.
  - CHECK: `cargo check -p arbkit-exec && cargo check -p arbkit-exec --features runner --example prod_trader`
  - EXPECT: `Finished` for both; the runner surface stays out of default builds.
  - EVIDENCE: both checks passed; `runner = ["live", "dep:arbkit-feed", "arbkit-feed/live", "dep:ureq"]` with `required-features` on the example.
- [x] HJ144-G2: Live mode refuses unsafe starts.
  - CHECK: code review of `examples/prod_trader.rs` guard clauses.
  - EXPECT: exit 3 while `ARBKIT_KILL_SWITCH` is engaged; exit 4 on missing credentials or an unanswered venue balance; exit 6 on unreconciled in-flight state; exit 5 on empty/failed discovery.
  - EVIDENCE: all five refusal paths present and ordered before any feed, engine, or order path starts.
- [x] HJ144-G3: A fixture-driven dry-run session runs end to end and degrades honestly.
  - CHECK: local fixture server serving Kalshi markets + Polymarket Gamma propositions; `prod_trader --mode=dry-run --windows=8` with dead ingest/command endpoints.
  - EXPECT: catalog builds, engine registers markets, session ends gracefully with exit 0; command-poll and stream failures are logged and non-fatal; dropped frames counted.
  - EVIDENCE: `catalog generation=1 events=1 active_pairs=1 paired_markets=1`; `engine registered 1 markets; subscriptions: kalshi=1 polymarket=1`; two dropped batches counted; `session ended: windows=8 attempted=0 ...`; state file written with `kill_switch: true` and empty `in_flight`.
- [x] HJ144-G4: Workspace hygiene holds with all features.
  - CHECK: `cargo fmt --all --check && cargo clippy --workspace --all-targets --all-features && cargo test --workspace --all-features`
  - EXPECT: clean.
  - EVIDENCE: fmt ok; clippy emitted zero warnings; every workspace test binary reported ok, including the new Kalshi ticker time-segment test (`26AUG261905BOSNYY-BOS` parses; current Kalshi tickers embed a 4-digit start time the grammar previously rejected, which is why unfiltered discovery skipped every market as malformed).
- [x] HJ144-G5: The runner reuses the frozen wire contract verbatim.
  - CHECK: `examples/prod_trader.rs` frame shapes vs `dashboard/worker/wire.ts` zod schemas.
  - EXPECT: `session-start`/`risk`/`positions`/`stats`/`heartbeat`/`session-end` with camelCase fields, integer cents, live extension fields stated; operator commands pulled through the shared `control.rs` included by path from the paper runner.
  - EVIDENCE: one command-protocol implementation shared with the paper runner; risk frames report every cap as `Some` because the runner genuinely enforces them.

## HJ-148 operator command plumbing gates

- [x] HJ148-G1: The worker edge rejects a disarming command that lacks explicit confirmation.
  - CHECK: `npm --prefix dashboard test -- --run` (`position-room.test.ts`)
  - EXPECT: `{t:"kill-switch",engage:false}` fails `operatorCommandSchema`; `{engage:false,confirm:true}` passes; unknown tags, bad modes, and non-boolean engage still rejected with 400 before the queue.
  - EVIDENCE: schema refine pins `engage || confirm === true`; all three suites assert both acceptances and rejections; 99 dashboard tests pass.
- [x] HJ148-G2: The console cannot send a disarm without the confirmation gesture, and still fails inert.
  - CHECK: `OperatorConsole.test.tsx`
  - EXPECT: Disarm disabled until the confirm checkbox is checked; clicking sends exactly `{t:"kill-switch",engage:false,confirm:true}`; disconnected renders offer no controls even when cached state reads disarmed.
  - EVIDENCE: payload assertion added to the disable/enable suite; inert-render suites unchanged and passing.
- [x] HJ148-G3: The runner enforces the confirmation independently of the worker.
  - CHECK: code review of `prod_trader.rs` and paper-runner command arms.
  - EXPECT: an unconfirmed disarm is refused and logged, never applied to `risk.config.kill_switch`; defense in depth against a worker-side defect.
  - EVIDENCE: both runners `continue` on `!engage && !confirm` with a REFUSED log line; only confirmed disarms mutate risk state.
- [x] HJ148-G4: Kill-switch applications are recorded with timestamp, identity, and command id.
  - CHECK: runbook log line format in `prod_trader.rs`.
  - EXPECT: `[utc] kill-switch engage=<bool> applied (operator command id=N operator=ID)` where identity comes from `ARBKIT_OPERATOR_ID` (default `unknown-operator`, never fabricated).
  - EVIDENCE: format pinned in code; `.env.example` documents `ARBKIT_OPERATOR_ID`.
- [x] HJ148-G5: A `session-start` mid-process cannot change the venue profile silently.
  - CHECK: code review of the `SessionStart` arm in `prod_trader.rs`.
  - EXPECT: a start whose mode matches the process mode is acknowledged as already running; a mismatched mode is refused with the running mode named (restart required to switch profiles).
  - EVIDENCE: both branches logged distinctly; one process = one session preserved.

## HJ-146 cross-venue market catalog gates

- [x] HJ146-G1: The team table resolves every live venue identifier on both sides.
  - CHECK: `cargo test -p arbkit-match --all-features`
  - EXPECT: all 30 live Kalshi MLB codes resolve with a sport hint (two-letter codes included); all 30 official Polymarket labels resolve uniquely hint-free; city names shared inside one league never silently pick one franchise.
  - EVIDENCE: roster built from captured API bodies (`tests/fixtures/`); `mlb_roster_resolves_every_live_kalshi_code_and_poly_label`, `mlb_shared_city_labels_stay_unresolvable`.
- [x] HJ146-G2: Ticker grammar decodes the live year-first stamp and rejects impossible ones.
  - CHECK: `cargo test -p arbkit-match --lib event_datetime_decodes_year_first`
  - EXPECT: `[YY][MMM][D]{1,2}[HHMM][codes]` decodes to canonical `YYYY-MM-DD` including single-digit days; hour>23, minute>59, day>31, unknown month, non-numeric year are malformed, never reinterpreted.
  - EVIDENCE: pre-live fixtures were palindromic (`26AUG26`) and hid the DDMMMYY vs YYMMMDD confusion; real slates broke it; regression tests pin both readings apart (`241840` → day 24 at 18:40).
- [x] HJ146-G3: Propositions bind by label, demand a dated fixture, and never double-bind a market.
  - CHECK: `cargo test -p arbkit-feed --features live`
  - EXPECT: reversed outcome listings still resolve; an outcome label outside the title's pair is skipped as inconsistent; missing/unparsable `gameStartTime` is malformed; same clubs on two dates pair only on the exact date; a second proposition for one canonical market is counted and dropped instead of overwriting.
  - EVIDENCE: `polymarket_page_resolves_at_vs_and_rejects_inconsistent_labels`, `same_clubs_on_two_dates_pair_by_exact_date`, `listing_order_never_flips_sides_and_duplicate_bindings_skip`.
- [x] HJ146-G4: Real captured API bodies populate a working catalog offline.
  - CHECK: `cargo test -p arbkit-feed --features live live_slate_fixture -- --nocapture`
  - EXPECT: ≥40 open Kalshi moneylines parsed from the stored `/markets` page; ≥20 canonical events; ≥10 validated pairs.
  - EVIDENCE: actual run — events=41 pairs=41 paired_markets=41 (skips counted, none guessed).
- [x] HJ146-G5: The production runner builds this catalog against the live public APIs.
  - CHECK: `prod_trader --mode=dry-run` with series/tag-scoped discovery URLs.
  - EXPECT: `catalog generation=1 events=N active_pairs=N` with N>0; zero-pair refusal intact; dead ingest/command endpoints degrade to retries without aborting discovery.
  - EVIDENCE: 2026-08-24 smoke — events=42 active_pairs=42 paired_markets=42.

## HJ-149 credential hygiene gates

- [x] HJ149-G1: The repo never ships a credential value.
  - CHECK: `cargo test -p arbkit-exec --test secret_hygiene`
  - EXPECT: every credential field in `.env.example` ships blank; the test fails if a default creeps in.
  - EVIDENCE: `env_example_keeps_credential_values_blank`.
- [x] HJ149-G2: Adapter logs cannot leak key material.
  - CHECK: same test file.
  - EXPECT: `KalshiConfig` Debug renders exactly two `[redacted]` fields (api key, PEM); `PolymarketConfig` renders four (L1 key, api key, secret, passphrase) and shows only public identifiers.
  - EVIDENCE: `adapter_debug_output_redacts_every_secret`.
- [x] HJ149-G3: Artifacts are swept for credential material before transmission and at shutdown.
  - CHECK: code review of `prod_trader.rs` + scanner tests.
  - EXPECT: sweeps run after state restore, once the journal exists and before order flow, and after the final checkpoint; a hit exits with code 9 naming artifact + label; dry-run sweeps whatever the environment holds (typically the stream token).
  - EVIDENCE: `assert_artifacts_clean` call sites; `SecretScan` unit tests include the negative control (a planted leak must be found or clean passes prove nothing).
- [x] HJ149-G4: The signing key cannot be read by group or others.
  - CHECK: code review of `live_endpoints()`.
  - EXPECT: on unix, mode bits `0o077` set on `KALSHI_PRIVATE_KEY_PATH` refuse startup before the file is parsed.
  - EVIDENCE: permission gate with operator guidance to mount `0600`.

## HJ-150 durable risk state gates

- [x] HJ150-G1: The stored risk policy wins on restart; the kill switch stays environmental.
  - CHECK: `cargo test -p arbkit-exec --test restart_drill`
  - EXPECT: limits (`stake`, `daily_loss_cap`, `open_trades_cap`, `min_edge`) restore from the snapshot even when env disagrees; drift is printed by the runner, never silently applied; `kill_switch` follows the live environment.
  - EVIDENCE: `effective_config` merge pinned in the drill; prod_trader prints a named drift line.
- [x] HJ150-G2: Orders are registered durably before submission and never transmitted unprotected.
  - CHECK: code review of `prod_trader.rs` execution arm.
  - EXPECT: both legs persist via `register_inflight` before any POST; a failed or partial persist rolls back the already-persisted legs and skips the plan (counted as `unprotected_skips`).
  - EVIDENCE: rollback loop + counter in the session-end summary.
- [x] HJ150-G3: Settlements reconcile by client order id with idempotent application, then leave no ghost.
  - CHECK: same drill test.
  - EXPECT: acknowledged leg settles exactly once across re-polls; `clear_inflight` removes it from durable state so healthy sessions never strand orders that block future live restarts.
  - EVIDENCE: drill asserts one settlement, empty re-poll, cleared store entry.
- [x] HJ150-G4: A checkpoint can never erase crash-recovery state.
  - CHECK: same drill test (the core acceptance line).
  - EXPECT: `checkpoint(&gate)` — whose gate knows nothing about in-flight — preserves the unacknowledged order in the store while merging live loss/bankroll fields.
  - EVIDENCE: explicit assertion after checkpoint with the poly leg still present.
- [x] HJ150-G5: The full rehearsal restores gate, bankroll, loss, and open trades exactly.
  - CHECK: `cargo test -p arbkit-exec --test restart_drill`
  - EXPECT: reload after simulated crash yields identical bankroll map, daily loss 300c, open trades 1, config equal to stored policy; restored gate's snapshot equals the loaded bankroll.
  - EVIDENCE: `restart_restores_gate_exactly_and_reconciles_idempotently`.

## HJ-151 dry-run warmup gates

- [x] HJ151-G1: The warmup records a raw tape and a human-verifiable catalog.
  - CHECK: live dry-run command in LIVE_TRADING.md § Dry-run warmup.
  - EXPECT: `--tape` captures every feed event crossing the bridge (binary, replayable); `--dump-catalog` writes one CSV row per leg with the Kalshi ticker and Polymarket decimal token id exactly as Gamma publishes it.
  - EVIDENCE: warmup run recorded real Polymarket events (`tape_events=52`); `poly_token_id_to_decimal` round-trip test pins the id rendering.
- [x] HJ151-G2: Feeds actually connect — TLS and signed market-data auth.
  - CHECK: `cargo test -p arbkit-feed --features live --lib ws_signer` + warmup run.
  - EXPECT: tungstenite built with rustls; Kalshi WS handshake signs `ts+GET+path` RSA-PSS like REST (verifiable signature pinned by test); missing credentials fail loudly with 401 rather than silently delivering an empty book.
  - EVIDENCE: this gate found that feeds had NEVER delivered an event in production config (`TLS support not compiled in`) and that Kalshi's socket requires auth; both fixed.
- [x] HJ151-G3: The warmup ledger is printed and pass-conditions are explicit.
  - CHECK: code review of session-end summary + LIVE_TRADING.md criteria.
  - EXPECT: `warmup ledger: unwind_failures=… ack_matched=… in_flight_remaining=… tape_events=…`; every acknowledged venue order id maps to its client order id by construction; open-but-settling orders are distinguished from orphans.
  - EVIDENCE: runner prints the line; drill (HJ150-G3) pins idempotent settlement and ghost clearing.
- [x] HJ151-G4: Failure-drill suite green on the deployed adapter build.
  - CHECK: `cargo test -p arbkit-exec --all-features --test failure_drills`
  - EXPECT: all drills green including `demo_and_sandbox_presets_point_at_test_environments_without_io`.
  - EVIDENCE: workspace suite green across repeated runs this ticket.

## HJ-153 runbook and falsification-record gates

- [x] HJ153-G1: The operator runbook covers every required path.
  - CHECK: read `RUNBOOK.md` against the pre-capital checklist in LIVE_TRADING.md.
  - EXPECT: session start (posture, scoped discovery, startup-line meanings, mapping spot-check), stop (natural/operator/hard-kill semantics), kill-switch arm/disarm with the audit log line, stale-feed detection and response, stuck-unwind procedure (never edit state), restart recovery (exit 6 + policy continuity), artifact locations table.
  - EVIDENCE: `RUNBOOK.md`; every procedure cites its pinning test or drill.
- [x] HJ153-G2: Runbook paths rehearsed on the deployed build.
  - CHECK: RESULTS.md §9 rehearsal rows.
  - EXPECT: engaged kill-switch live start refused with exit 3; a full start/stop cycle ends clean; all other paths cite their standing drills (`restart_drill`, `failure_drills`, HJ148 suites).
  - EVIDENCE: `live mode refused … exit 3`; `windows=4 attempted=0 unwind_failures=0 in_flight_remaining=0`.
- [x] HJ153-G3: Dated falsification records exist and include negative findings.
  - CHECK: read RESULTS.md §9.
  - EXPECT: per-session dated rows (including the honest `attempted=0` baseline) plus a five-row falsified-assumptions table — ticker grammar, status field, team-code split, missing TLS, Kalshi WS auth — each with its fix and regression pin.
  - EVIDENCE: §9 session log + falsified-assumptions table.

## HJ-152 micro-live gates

- [x] HJ152-G1: Micro-live caps are enforced in code, not in prose.
  - CHECK: `cargo test -p arbkit-exec --lib --all-features micro_live_caps`
  - EXPECT: `--micro` clamps per-leg stake to two-contract worst-case (200¢) and daily budget to one leg loss (200¢); tighter operator env values are never loosened.
  - EVIDENCE: `micro_live_config` + rehearsal boot line `max_stake_per_leg=200c daily_loss_cap=200c`.
- [x] HJ152-G2: Every executed signal freezes a detection-time occurrence.
  - CHECK: code review of the execution arm + dry-run artifacts.
  - EXPECT: `occurrences.ndjson` gains one record per transmitted plan (edge, worst-case profit, legs with quoted ppm/fee/stake), written before results are known.
  - EVIDENCE: runner writes records at execution; `occurrence_record` pairs plan entries to legs via client-order-id venue/outcome bytes.
- [x] HJ152-G3: Graceful shutdown emits the live proof report.
  - CHECK: dry-run session artifact.
  - EXPECT: `live-proof.json` carries attempted/fills/phantoms/unwinds/theoretical/realized (settled totals from the idempotent ledger); session-end prints the exact compare command.
  - EVIDENCE: artifact inspected after rehearsal; counters zero-honest on a no-trade session.
- [x] HJ152-G4: Same-tape comparison halts on phantom blowout.
  - CHECK: `cargo test -p arbkit-exec --features paper-replay --test same_tape_proof`
  - EXPECT: compare exits `0` within tolerance, `1` on ROI falsification, and **`2`** when live phantoms exceed the paper baseline by >10pp — re-arm-and-explain semantics, machine-readable.
  - EVIDENCE: `phantom_rate_beyond_ten_points_halts_micro_live` pins the gate; boundary case (1/20) stays green.

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
