# arbkit-dashboard

[![Live Demo](https://img.shields.io/badge/Live%20Demo-arbkit.harlanljones.com-0ea5e9?style=flat&logo=cloudflare)](https://arbkit.harlanljones.com/)
[![License](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue.svg)](../README.md#license)

The interactive proof ledger and benchmark results visualizer for the [`arbkit`](../README.md) Rust workspace.

> 🌐 **Production Deployment:** [https://arbkit.harlanljones.com/](https://arbkit.harlanljones.com/)

---

## Features

- **Audited Proof Ledger:** Visualizes the p99 latency against the 50 µs budget ruler with exact headroom calculation.
- **Latency Distribution Plot:** Tail percentiles (p50, p90, p99, p99.9, p99.99, max) across dated benchmark snapshots.
- **Host-to-Host Throughput:** High-frequency burst ingestion comparison across architectures (Apple Silicon and Linux x86_64).
- **Execution & Phantom Breakdown:** Proportional fills vs. queue-decayed phantom signals.
- **Pessimistic Financial Accounting:** Realized worst-case settlement PnL computed strictly in integer cents.
- **Workspace Verification Matrix:** Live pass/fail and clippy status across all workspace crates.
- **Operator Console:** Session start/end, kill-switch arm/disarm, risk-envelope display, open positions, and fill reconciliation — driven through an authenticated command channel, failing inert whenever the stream is down.

---

## Development

```bash
# Install dependencies
npm install

# Run local development server
npm run dev

# Run unit and component tests
npm test

# Run end-to-end tests
npm run test:e2e

# Build production bundle
npm run build
```

---

## Recording a Benchmark Snapshot

To execute the Rust release pipeline on the local machine, validate the resulting JSON report schema, and append a versioned snapshot to `public/data/runs/`:

```bash
npm run record
```

Review the updated `public/data/runs/index.json` and generated snapshot file before committing.

---

## Cloudflare Workers Deployment

The dashboard builds as a static Single Page Application (SPA) served by the
canonical `arbkit` Cloudflare Worker:

```bash
# Dry run deploy
npm run deploy:dry

# Deploy to Cloudflare Workers
npm run deploy
```

Configuration is defined in [`wrangler.jsonc`](wrangler.jsonc).

---

## Live Position Stream

Beyond the static proof ledger, the worker hosts a live trading view:
`worker/index.ts` routes `/api/live/*` to the single `PositionRoom` Durable
Object, which owns all session arithmetic (totals, ROI bps floored toward
negative infinity, disposition funnel), relays the runner's authoritative
risk posture and fill reconciliation, and pushes validated frames to any
number of read-only WebSocket viewers.

![Live stream overview with the operator console](../docs/screenshots/live-stream-overview.png)

- **Producer:** `cargo run -p arbkit-engine --example live_runner -- --url <ingest-url>`
  streams detected-and-settled positions from the pipeline workload in fixed
  wall-clock windows (see the example's docs for flags). The token comes from
  `LIVE_INGEST_TOKEN` in the environment — `set -a; source .dev.vars; set +a`
  is enough locally (`--token-env VAR` names any other variable).
- **Ingest auth:** `LIVE_INGEST_TOKEN` — set locally via `.dev.vars`
  (see [`.dev.vars.example`](.dev.vars.example)), in production via
  `wrangler secret put LIVE_INGEST_TOKEN`.
- **Viewer:** `wss://<host>/api/live/ws`, public and read-only; clients may
  send `{ "t": "resume", "afterSeq": N }` to replay missed ledger rows.
- **Staleness:** a session whose heartbeats stop for 20 s is marked stale by
  alarm and stays visible as such until its runner returns or a new session
  opens.

---

## Operator Console

The live view carries an operator-facing control surface for supervised
sessions. Every control is downstream of the runner's own authority: a
command is only ever *queued* by the dashboard, applied by the runner's risk
gate, and confirmed by the runner's next `risk` frame.

![Operator console with the kill switch engaged](../docs/screenshots/operator-console-engaged.png)

![Operator console with a disarmed running session](../docs/screenshots/operator-console-disarmed.png)

- **Command endpoint:** `POST /api/live/command`, authenticated by
  `LIVE_OPERATOR_TOKEN` — a separate secret from the ingest token on purpose:
  holding the runner's push credential must not confer the right to command.
  Set it via `.dev.vars` locally and `wrangler secret put LIVE_OPERATOR_TOKEN`
  in production. Bodies are zod-validated at the worker edge (`session-start`
  with explicit mode, `session-end`, `kill-switch`); malformed commands are
  refused with 400 before they reach the queue.
- **Runner pull:** the runner fetches queued commands from
  `GET /api/live/commands?afterId=<highWater>` with its ingest credential.
  Delivery is at-least-once and every command applies idempotently.
- **Fail-inert posture:** an unknown kill switch reads as engaged; a
  disconnected console renders its last-known state but every control goes
  inert; a fresh session resets posture until the new runner re-declares it.
- **Risk envelope:** per-leg stake cap, daily loss budget consumed vs.
  remaining, open-trade count, and edge floor are displayed verbatim from the
  runner's `risk` frame. A cap the runner does not enforce renders as "not
  enforced" — never replaced with a client-side default.
- **Positions and fills:** open positions show locked filled stake, venue
  order IDs, and settlement status; realized cents appear only once
  settlement reports them. The reconciliation feed lists fills keyed by
  client/venue order ID as the ledger absorbs them.

The screenshots above are regenerated from scripted worker frames against the
real built app — no hand-edited pixels:

```bash
npm run shots   # builds, then writes ../docs/screenshots/*.png
```
