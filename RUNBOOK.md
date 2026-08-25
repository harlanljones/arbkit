# Operator runbook — `prod_trader`

Operating procedures for the assembled live process. This runbook exists to
satisfy the pre-capital checklist in `LIVE_TRADING.md` ("Runbook covers: …");
it is an operations document, not evidence that live orders have been placed.
Rehearsal status for every path lives in `RESULTS.md` §9.

One process is one session. Every path below assumes the safety posture of
`LIVE_TRADING.md`: kill switch engaged at rest, `DryRunAdapter` unless live is
explicit, secrets from a secret manager only.

---

## 0. Posture check (before any session)

```bash
# Resting posture: switch engaged, limits deliberate, no credentials in shell.
grep -E 'ARBKIT_KILL_SWITCH|ARBKIT_MAX|ARBKIT_MIN_EDGE' .env   # must NOT exist
echo "ARBKIT_KILL_SWITCH=${ARBKIT_KILL_SWITCH:-<unset>}"        # unset == engaged
```

Feed credential presence — presence only, never values (a dry-run warmup needs
the read-only Kalshi signing key to form a Kalshi book; without it the feed
401s loudly on connect and only the Polymarket side of the tape exists):

```bash
if [ -n "${KALSHI_ACCESS_KEY_ID:-}" ]; then
  echo "KALSHI_ACCESS_KEY_ID: present"
else
  echo "KALSHI_ACCESS_KEY_ID: ABSENT (Kalshi book will 401)"
fi
if [ -n "${KALSHI_PRIVATE_KEY_PATH:-}" ] && [ -r "$KALSHI_PRIVATE_KEY_PATH" ]; then
  echo "KALSHI_PRIVATE_KEY_PATH: present, readable"
else
  echo "KALSHI_PRIVATE_KEY_PATH: ABSENT or unreadable (Kalshi book will 401)"
fi
```

- Kill switch engaged (`ARBKIT_KILL_SWITCH` unset or != `0`) is the only
  acceptable resting state.
- Live mode refuses to start while engaged (exit code 3). Never "temporarily"
  export `ARBKIT_KILL_SWITCH=0` into a shared shell; arm per-session, on
  purpose (§2).
- The boot line prints `kalshi_feed_signed=true|false`; a warmup that needs
  both venues' books must see `true` before starting (§1). Never echo the
  key id or key path value — presence is the only signal the posture check
  reads (finding F3).

## 1. Session start / stop

**Start (dry-run rehearsal or paper):**

```bash
cargo run -p arbkit-exec --features runner --example prod_trader --release -- \
    --mode=dry-run \
    --kalshi-markets-url='https://api.elections.kalshi.com/trade-api/v2/markets?series_ticker=KXMLBGAME' \
    --poly-events-url='https://gamma-api.polymarket.com/events?tag_slug=mlb' \
    [--url=http://127.0.0.1:8787/api/live/ingest] [--token-env=LIVE_INGEST_TOKEN] \
    [--state=prod-risk-state.json] [--journal=prod-session.ndjson] \
    [--window-ms=250] [--windows=<n>]
```

- Discovery runs once at boot; an empty catalog **refuses to run**. Scope the
  discovery URLs (as above) — the runner appends its own `status=open` /
  `closed=false&limit…` filters, so a pinned URL must carry only the series
  or tag; duplicating a filter 400s at the venue. Unscoped defaults page
  through every open market on both venues and are noise.
- Kalshi's market-data socket is authenticated even for read-only books:
  without `KALSHI_ACCESS_KEY_ID` + `KALSHI_PRIVATE_KEY_PATH` in the session
  environment the Kalshi feed 401s and never forms a book (Polymarket's is
  anonymous). Dry-run still starts, streams, and rehearses the command path —
  a full-slate warmup needs those read-only credentials supplied.
- Flags accept both `--flag=value` and `--flag value`; a value-taking flag
  written bare with no following value is a usage error (exit 2), never a
  silent default.
- Without `--windows`, the session runs until `session-end` arrives through
  the command queue (§3) or the process is killed.
- Boot line prints mode + full risk posture; the first `risk` frame is the
  authoritative statement of what governs this session.

**Stop (graceful):** send `session-end` from the operator console (or POST it
to `/api/live/command`). The runner drains, checkpoints risk state one final
time, emits `Frame::SessionEnd`, exits.

**Stop (kill):** killing the process skips all shutdown — that is allowed and
the dashboard declares staleness by heartbeat timeout. Recovery is §5's job;
do not treat a killed session as an emergency by itself.

## 2. Kill-switch arm / disarm

- **Resting:** engaged. Dry-run sessions run *with* the switch engaged — the
  gate records posture; dry-run transmits nothing regardless.
- **Arm (live only):** set `ARBKIT_KILL_SWITCH=0` in the session's own
  environment (secret-manager wrapper, not shell history), then start. A live
  boot with the switch engaged exits 3 before touching credentials.
- **Disarm/engage at runtime:** operator console → kill switch. Disarm
  requires explicit confirmation twice over: the worker's schema rejects a
  bare disarm with `400`, and the runner independently refuses one
  (`REFUSED disarm command … no explicit confirmation` in the log).
- **Every applied flip** logs `[UTC] kill-switch engage=<bool> applied
  (operator command id=<id> operator=<ARBKIT_OPERATOR_ID>)`. That line in
  `prod-session.ndjson` / stderr is the audit record; no console message is.
- **Loss of control plane never stops trading** — only the switch does. A
  disconnected console fails inert: view state, act through nothing.

## 3. Stale-feed response

The feed layer halts on parser sequence gaps: a stale event suppresses
signals engine-wide until a fresh snapshot restores the book. Operator
response:

1. Confirm from the dashboard heartbeat/staleness banner and the runner's
   stderr — do not restart reflexively; a restart during open positions is
   §5 territory with more moving parts.
2. Check the feed reconnects on its own (connectors reconnect after transport
   failures by design). Staleness that clears itself needs no action beyond a
   journal note.
3. Staleness persisting past one window cycle: stop order-flow expectations,
   then either wait out the venue outage or `session-end` cleanly. Note the
   window in the session record.
4. Never widen tolerance, lower `min_edge_bps`, or otherwise tune around a
   stale tape mid-session.

## 4. Stuck-unwind reconciliation

A hedge that rejects/partially fills unwinds its accepted leg. An unwind that
*itself* fails holds capital conservatively rather than pretending flatness
(`drill_failed_unwind_conservatively_holds_capital` pins this).

1. Read `prod-risk-state.json` → `in_flight` entries; read the journal tail
   for the matching `client_order_id`, classification, and venue order ids.
2. Poll the venue for the accepted leg's true state. Terminal fills apply
   exactly once through the idempotent reconciliation ledger (keyed by client
   order id) — re-running reconciliation after a crash cannot double-count.
3. Manually flatten anything still live at the venue with the venue's own
   tools; record the venue order id and fill in the session notes.
4. Only when `in_flight` is empty may the next session start in live mode:
   the runner refuses to restart live over unreconciled orders.
5. If the drill suite is not green on the deployed build
   (`cargo test -p arbkit-exec --test failure_drills`), stop-the-line.

## 5. Restart recovery

1. Restart with the **same** `--state` path. The stored snapshot carries the
   risk policy that governed money at risk; its limits win over this run's
   environment, any drift is printed — never silently applied. The kill
   switch stays governed by live env (posture is now, policy was then).
2. In-flight orders re-seed from durable state and reconcile per §4; the
   restart drill (`restart_drill.rs`) pins exact restoration, idempotent
   settlement by client order id, and that a checkpoint can never erase
   unacknowledged recovery state.
3. Changing venue profile/mode requires a fresh process decision — a
   mismatched `session-start` is refused by name, not silently honored.

## 6. Artifact locations & secret sweep

| Artifact | Written by | Contents |
| --- | --- | --- |
| `prod-risk-state.json` | runner checkpoints | bankroll, caps actually in force, in-flight ledger |
| `prod-session.ndjson` | runner journal | execution records + operator command audit lines |
| dashboard frames | worker ingest | `session-start`/`risk`/`positions`/`stats`/`heartbeat`/`fills` |
| `occurrences.ndjson` | runner (per executed signal) | detection-frozen occurrence tape — the paper side of same-tape proof |
| `live-proof.json` | runner (graceful shutdown) | live counters for the same attempts; exit codes below |
| `warmup-tape.bin`, `catalog-dump.csv` | runner (`--tape`, `--dump-catalog`) | raw feed replay + human-verifiable pair mapping |

After every session run the comparison (exit `0` within tolerance, `1`
falsified ROI, `2` phantom-rate halt):

```bash
cargo run -p arbkit-exec --features paper-replay --example same_tape_proof -- \
    --input occurrences.ndjson --compare live-proof.json --tolerance-bps 50
```

```bash
grep -rF -e "$KALSHI_ACCESS_KEY_ID" -e "$POLY_API_SECRET" \
    prod-session.ndjson prod-risk-state.json occurrences.ndjson live-proof.json
```

Any match is a leak and a stop-the-line event (runner also self-sweeps with
exit code 9 naming the artifact and label, never the value). Every listed
file may be absent when the corresponding tool never ran.

## 7. Micro-live posture

Start with `--micro` to clamp policy before anything transmits:

```bash
... prod_trader -- --mode=live --micro --state=micro-state.json \
    --occurrences=occurrences.ndjson --proof=live-proof.json
```

- Per-leg stake capped to 200¢ — two contracts at any legal binary price
  (≤ 99¢) can never exceed it, whatever the book quotes.
- Daily loss budget clamped to the same figure: one worst-case leg loss,
  never more.
- Clamps only tighten; an operator's smaller env values always win. After
  the first micro session the stored policy keeps these caps (§5) until an
  operator deliberately widens with a fresh state file.
- Disarm only via the console's confirmed gesture (§2); the runner refuses
  unconfirmed disarms independently.
- After every session run the §6 comparison and record the outcome as a
  dated row in RESULTS.md §9 — exit `2` means re-arm and explain.

## 8. Reporting rule

A negative live ROI or a phantom rate above the paper baseline is a **valid
finding**: it falsifies the synthetic assumption. Record it as a dated row in
`RESULTS.md`; never relabel, recompute, or widen tolerance until it passes.
