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
