# Issue tracker: Linear

Issues and specs for this repo live in **Linear**.

## Command and scope

- Command: `/home/harlan/.cache/.bun/bin/linear` (or `linear` on PATH)
- Workspace: `harlanljones`
- Team: `HJ` (Harlan Jones)
- Project: `Live trading prod readiness` — slug `741506c9d49e`, URL `https://linear.app/harlanljones/project/live-trading-prod-readiness-741506c9d49e`
  (the earlier completed phase lives in `arbkit live trading operations` — slug `ed6262e1f37c`)

Run the command's `--version` and `--help` once at the start of a tracker
session. The installed CLI's help is authoritative.

## States and labels

Linear workflow states and labels are separate. Triage roles such as
`ready-for-agent` are labels; applying one does not move workflow state unless
the invoking skill says to.

## Common operations

- Create: `linear issue create --no-interactive --team HJ --title "..." --description-file <path>`
- Read: `linear issue view [ID] --json --no-download`
- Query: `linear issue query --team HJ --all-states --all-assignees --json`
- Comment: `linear issue comment add [ID] --body-file <path>`
- Incremental labels: `linear issue update [ID] --add-label "..."` / `--remove-label "..."`
- Claim: `linear issue update [ID] --assignee self`
- Complete: `linear issue update [ID] --state completed`
- Assign to project: `linear issue update [ID] --project <projectId-or-name>`

Use Markdown files for multi-line descriptions and comments. Never print or
store the API token in the repository.

## Linear agent tracking

The production-readiness program for live trading was tracked as a parent
issue (`HJ-143`) with child tickets `HJ-144`–`HJ-153`, with native blocked-by
relations modeling the prerequisite order. All eleven issues belong to the
`Live trading prod readiness` project (slug `741506c9d49e`) and are **Done**
as of 2026-08-24; per-ticket evidence lives in each issue's closing comment,
in `GATES.md`, and in `RESULTS.md` §9.

### arbkit v0.2 (2026-08-24)

The **arbkit v0.2** project (slug `856235744930`,
https://linear.app/harlanljones/project/arbkit-v02-856235744930) tracks the
micro-live execution program and its operator-facing dashboard surface,
using the same parent-plus-children pattern: parent `HJ-288` with children

| Ticket | Blocked by |
|---|---|
| `HJ-289` flag parser accepts space-separated values (F1) | — |
| `HJ-290` read-only Kalshi feed credentials (F3) | — |
| `HJ-291` dashboard: micro-live session ledger panel | — |
| `HJ-292` dashboard: operator command audit trail UI | — |
| `HJ-293` dashboard: stream-stale / failing-inert banner | — |
| `HJ-294` first micro-live session under `--micro` caps | HJ-290 |
| `HJ-295` per-session same-tape comparison + dated rows | HJ-294 |
| `HJ-296` v0.2 readiness review + docs status flip | HJ-295 |

All nine issues start in Backlog, unassigned; evidence lands in
`RESULTS.md` §9 and `GATES.md`.

**Completed 2026-08-24.** The five engineering children (`HJ-289`–`HJ-293`)
are **Done**, each with a closing comment carrying its verification evidence
(workspace tests, clippy/fmt, dashboard suites). The operator-gated execution
chain (`HJ-294`–`HJ-296`: first micro-live session, same-tape cadence,
readiness review) was descoped from the agent-tracked program rather than
completed: executing it requires operator-injected venue credentials and an
explicit kill-switch decision that no agent session may make on the
operator's behalf, and no session evidence may be fabricated. The procedure
itself lives in `RUNBOOK.md` §7–§8 and `LIVE_TRADING.md`'s acceptance
criteria; when the operator runs it, evidence lands as dated `RESULTS.md` §9
rows per the standing reporting rule. Parent `HJ-288` closed **Done** with
the full program summary in its closing comment.

### arbkit auth control (2026-08-24)

The **arbkit auth control** project (slug `9c014213ffe9`,
https://linear.app/harlanljones/project/arbkit-auth-control-9c014213ffe9)
replaces anonymous shared-token authority on the live surfaces with
per-operator authenticated sessions backed by Kalshi identity, attributed
end to end (console → worker → runner log). Parent `HJ-308` with children:

| Ticket | Blocked by |
|---|---|
| `HJ-309` decide the Kalshi login mechanism and fallback | — |
| `HJ-310` console session model (login, roles, revocation) | HJ-309 |
| `HJ-311` worker edge binds commands to sessions; identity on the wire | HJ-310 |
| `HJ-312` runner verifies command identity; attribute applied actions | HJ-311 |
| `HJ-313` demote the shared operator token; rotation story | HJ-311 |
| `HJ-314` verified identity in the command audit trail UI | HJ-311 |
| `HJ-315` verification pass and docs flip (close-out) | HJ-312 + 313 + 314 |

Decision-first: `HJ-309` is the only frontier ticket until the mechanism is
chosen against primary-source evidence. Invariant carried on every ticket:
the dashboard queues commands and never holds venue trading credentials or
order authority.

**Completed 2026-08-24.** Decision (HJ-309, dated comment with linked
evidence): Kalshi exposes no OAuth/OIDC surface — login is signed-challenge
session linking against an operator roster. Shipped: session model
(HJ-310), command authentication inside the room with identity on the wire
(HJ-311), runner attribution incl. self-reported fallback labeling
(HJ-312), operator token demoted to documented break-glass with RUNBOOK §9
access-control procedures (HJ-313), and verified issuer rendered in the
command trail (HJ-314). Close-out verification in HJ-315's closing comment.
The shared operator token's final removal lands with the console login UI.
