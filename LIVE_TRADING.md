# Live trading integration

This document describes the opt-in execution boundary added to `arbkit`. It
is an implementation guide, not evidence that live orders have been placed.
The repository still treats paper results and live results as separate claims.

## Safety model

The detector and engine remain network-free. Live I/O begins only after a
`SignalEvent` has crossed the engine's signal ring:

```text
WebSocket feeds -> FeedEvent ring -> Engine -> SignalEvent ring
                                                  |
                                                  v
                                           RiskGate -> HedgedExecutor
                                                        |
                                              venue adapters / ledger
```

The default posture is safe:

- `arbkit-feed` live connectors are behind the `live` feature.
- `arbkit-exec` defaults to `DryRun`.
- `RiskConfig::default().kill_switch` is `true`.
- The example binary refuses live mode while `ARBKIT_KILL_SWITCH` is active.
- No credentials are stored in Rust source, `.env.example`, or the dashboard.
- `arbkit-core` and the engine hot loop do not depend on Tokio, HTTP, or venue
  authentication.

To enable a real deployment, an operator must explicitly provide credentials,
construct venue adapters, set conservative limits, validate the market catalog,
and disable the kill switch. That operational step is intentionally not part
of CI or the default example.

## Crate responsibilities

### `arbkit-feed`

With `--features live`, the crate exposes:

- `KalshiLiveFeed`, subscribing to Kalshi order-book deltas.
- `PolymarketLiveFeed`, subscribing to Polymarket market-channel updates.
- `MpscFeedBridge` and `spawn_ring_bridge`, which move parsed `Copy`
  `FeedEvent` values from async tasks to the synchronous SPSC producer.

The connectors reconnect after transport failures. Parser sequence gaps emit a
halt/stale event so the engine suppresses signals until a fresh snapshot
restores the book. Tape recording remains available at the feed boundary for
same-tape paper/live comparison.

### `arbkit-match`

`VenueInstrumentMap` is a startup-time catalog. A pair becomes active only if:

1. Both instruments resolve to the same canonical market.
2. They belong to different venues.
3. Their `MarketKind` values match.
4. Their `OutcomeSide` values are exact opposites according to
   `validate_binary_pair`.

Polymarket decimal token IDs are converted to a fixed `[u8; 32]` form by
`parse_poly_token_id`; no floating-point conversion is used. Unmatched or
ambiguous markets must be omitted from the active map and are never eligible
for execution.

### `arbkit-exec`

The execution crate contains the application-boundary contracts:

- `ExecLeg` — venue, instrument, limit price, stake, and idempotency key.
- `RiskConfig` — per-leg stake cap, daily loss cap, open-trade cap, edge floor,
  and kill switch.
- `RiskGate` — reserves per-venue capital before submission and releases it on
  a failed hedge.
- `VenueAdapter` — the seam for a venue's authenticated FOK/IOC API.
- `DryRunAdapter` — deterministic, no-I/O adapter for tests and warm-up runs.
- `HedgedExecutor` — submits both legs, requires complete fills, and unwinds
  accepted legs when either side rejects or partially fills.
- `LiveProofReport` and `compare_tape` — integer-cent/basis-point proof data.

Adapters own authentication, request signing, venue order IDs, and venue-
specific unwind behavior. They should never be called from the engine thread.
Every live adapter must preserve idempotency using `ExecLeg::client_order_id`.

## Execution lifecycle

```text
1. Receive SignalEvent from the signal ring.
2. Convert its plan into exactly two ExecLeg values.
3. Validate edge, caps, loss budget, open-trade count, and bankroll.
4. Reserve both venue balances.
5. Submit both FOK/IOC orders at the detected limit prices.
6. If both fully fill, emit a live record with settlementStatus=open.
7. Otherwise cancel/flatten every accepted leg and emit live_phantom evidence.
8. Reconcile venue fill/settlement streams and update authoritative cents.
```

The current `HedgedExecutor` is deliberately synchronous at its trait seam so
it can be tested without a runtime. A production runner should call it from a
Tokio execution task and implement adapters with concurrent requests; the
engine contract does not change.

## Dashboard wire contract

Live records extend the existing trade record with optional fields:

| Field | Meaning |
| --- | --- |
| `executionMode` | `paper` or `live` |
| `venueOrderIds` | Venue-assigned order IDs for accepted legs |
| `filledStakeCents` | Authoritative filled stake |
| `settlementStatus` | `open`, `settled`, or `unwound` |
| `realizedProfitCents` | Integer cents; nullable until settlement |

The worker accepts and relays these fields. It does not reconstruct live PnL
from prices or recompute venue fees. The dashboard displays an explicit
“Live Trading: real capital, not synthetic” banner when a live record is
present; paper sessions retain their existing synthetic-workload warning.

## Configuration

Copy `.env.example` into an operator-managed environment and replace blanks
with secrets supplied by a secret manager. Important defaults are:

```text
ARBKIT_KILL_SWITCH=1
ARBKIT_MAX_STAKE_PER_LEG_CENTS=5000
ARBKIT_MAX_DAILY_LOSS_CENTS=50000
ARBKIT_MAX_OPEN_TRADES=1
ARBKIT_MIN_EDGE_BPS=50
```

The example command is intentionally a policy check:

```bash
cargo run -p arbkit-exec --example live_trader -- --mode=dry-run
```

It does not create venue clients or place orders. A production runner must
add catalog loading, feed tasks, signal consumption, authenticated adapters,
fill reconciliation, and dashboard streaming as one separately reviewed
deployment change.

## Proof protocol

1. Run live feeds in dry-run for at least one representative slate.
2. Record the raw feed tape and verify mappings manually.
3. Run micro-stakes only after catalog and bankroll checks are reviewed.
4. Record attempted arbs, complete fills, phantoms, unwinds, fees, slippage,
   filled stake, and settled profit in integer cents.
5. Replay the same tape through the paper simulator.
6. Compare paper and live ROI in basis points using `compare_tape`.

A negative live ROI or high phantom rate is a valid result. It falsifies the
synthetic assumption and should be reported rather than hidden by recomputing
or relabeling the numbers.

## Verification

The implementation is covered by:

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
npm --prefix dashboard test -- --run
npm --prefix dashboard run build
npm --prefix dashboard run typecheck:worker
```

These checks use mocks, dry-run adapters, and local fixtures. They never place
live orders.

## Current boundary

The repository now provides the feed, catalog, risk, execution, proof, and
dashboard contracts. It does not claim production readiness: authenticated
Kalshi RSA-PSS and Polymarket L1/L2 adapters, live REST discovery, concurrent
venue submission, settlement subscriptions, and a fully wired production
`live_trader` process still require venue-specific integration and operator
review.
