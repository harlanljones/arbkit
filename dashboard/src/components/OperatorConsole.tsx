//! OperatorConsole: the operator-facing control surface for live sessions.
//!
//! Everything displayed here is adopted from the runner's own frames or from
//! explicit worker acks — nothing about authority is inferred client-side.
//! Two rules shape every control:
//!
//! * **Fail inert.** A disconnected console renders its last-known posture
//!   but offers no controls at all: cached state may be looked at, never
//!   acted through. An unknown kill switch reads as engaged.
//! * **Queued ≠ applied.** A 202 from the worker means the command was
//!   queued for the runner; the console says exactly that, and only the
//!   runner's next `risk` frame flips the authoritative pills.

import { useState } from "react";
import { money } from "../data/metrics";
import type { FillRecord } from "../data/liveSchema";
import type { TradeRecord } from "../data/schema";
import type { LiveSessionState } from "../data/liveSession";
import type { CommandAuditEntry, OperatorCommand, OperatorController } from "../data/useOperator";

const FILL_FEED_ROWS = 12;
/** Rows shown in the command trail, newest first. */
const AUDIT_VISIBLE_ROWS = 12;

export function OperatorConsole({
  live,
  operator,
}: {
  live: LiveSessionState;
  operator: OperatorController;
}) {
  const [mode, setMode] = useState<"paper" | "live">("paper");
  const [liveConfirmed, setLiveConfirmed] = useState(false);
  const [disarmConfirmed, setDisarmConfirmed] = useState(false);

  const risk = live.risk;
  // Unknown posture is engaged: the safe direction, always.
  const killSwitchEngaged = risk === null || risk.killSwitch;
  const connected = live.connection === "open";
  const commandsReachable = connected && !operator.pending;

  // Order entry exists only on an open connection, with the runner's own
  // report saying the switch is disarmed and a session actually running.
  const orderEntryOpen =
    commandsReachable && !killSwitchEngaged && live.sessionStatus === "live";

  const startDisabled =
    !commandsReachable ||
    killSwitchEngaged ||
    (mode === "live" && !liveConfirmed);
  const endDisabled = !commandsReachable || live.sessionStatus !== "live";
  const armDisabled = !commandsReachable || killSwitchEngaged;
  const disarmDisabled = !commandsReachable || !killSwitchEngaged || !disarmConfirmed;

  const openPositions = live.items.filter(
    (record) => record.settlementStatus === "open",
  );
  const settledPositions = live.items
    .filter((record) => record.settlementStatus === "settled" || record.settlementStatus === "unwound")
    .slice(-FILL_FEED_ROWS)
    .reverse();

  return (
    <section className="operator-console" aria-label="Operator console">
      <div className="operator-head">
        <strong>Operator console</strong>
        <span className={`live-pill ${killSwitchEngaged ? "live-pill--kill-engaged" : "live-pill--kill-disarmed"}`} data-testid="kill-switch-state">
          {killSwitchEngaged ? "Kill switch engaged" : "Kill switch disarmed"}
        </span>
        <span className="live-pill live-pill--mode" data-testid="execution-mode">
          Mode: {risk?.executionMode ?? "unknown"}
        </span>
        {!connected && (
          <span className="operator-note" role="alert">
            Console disconnected — controls are inert until the stream is live again.
          </span>
        )}
        {connected && killSwitchEngaged && (
          <span className="operator-note">
            Order entry is closed while the kill switch is engaged.
          </span>
        )}
      </div>

      <dl className="live-console-meta" data-testid="posture-summary">
        <div>
          <dt>Run</dt>
          <dd>{live.session ? shortRunId(live.session.runId) : "Awaiting session header"}</dd>
        </div>
        <div>
          <dt>Per-leg stake cap</dt>
          <dd>{formatCap(risk?.maxStakePerLegCents)}</dd>
        </div>
        <div>
          <dt>Daily loss budget</dt>
          <dd>{risk === null ? "unknown" : formatLossBudget(risk)}</dd>
        </div>
        <div>
          <dt>Open trades</dt>
          <dd>{formatPair(risk?.openTrades, risk?.maxOpenTrades)}</dd>
        </div>
        <div>
          <dt>Edge floor</dt>
          <dd>{formatEdgeFloor(risk?.minEdgeBps)}</dd>
        </div>
      </dl>

      <div className="operator-controls" data-testid="order-entry" data-open={orderEntryOpen ? "true" : "false"}>
        <fieldset className="operator-group" disabled={!commandsReachable}>
          <legend>Kill switch</legend>
          <label className="operator-confirm">
            <input
              type="checkbox"
              checked={disarmConfirmed}
              onChange={(event) => setDisarmConfirmed(event.target.checked)}
              disabled={!killSwitchEngaged}
            />
            Confirm disarming: real orders may flow
          </label>
          <button
            type="button"
            onClick={() => {
              setDisarmConfirmed(false);
              void operator.send({ t: "kill-switch", engage: false, confirm: true });
            }}
            disabled={disarmDisabled}
          >
            Disarm
          </button>
          <button
            type="button"
            onClick={() => void operator.send({ t: "kill-switch", engage: true })}
            disabled={armDisabled}
          >
            Arm
          </button>
        </fieldset>

        <fieldset className="operator-group" disabled={!orderEntryOpen}>
          <legend>Session</legend>
          <label>
            <input
              type="radio"
              name="operator-mode"
              checked={mode === "paper"}
              onChange={() => setMode("paper")}
            />
            Paper
          </label>
          <label>
            <input
              type="radio"
              name="operator-mode"
              checked={mode === "live"}
              onChange={() => setMode("live")}
            />
            Live — real capital
          </label>
          <label className="operator-confirm">
            <input
              type="checkbox"
              checked={liveConfirmed}
              onChange={(event) => setLiveConfirmed(event.target.checked)}
              disabled={mode !== "live"}
            />
            Confirm live mode before start
          </label>
          <button
            type="button"
            onClick={() => {
              setLiveConfirmed(false);
              void operator.send({ t: "session-start", mode });
            }}
            disabled={startDisabled}
          >
            Start session ({mode})
          </button>
          <button
            type="button"
            onClick={() => void operator.send({ t: "session-end" })}
            disabled={endDisabled}
          >
            End session
          </button>
        </fieldset>

        <p className="operator-ack" role="status">
          {operator.pending
            ? "Sending command…"
            : operator.lastError !== null
              ? `Last command refused: ${operator.lastError}`
              : operator.lastQueuedId !== null
                ? `Command queued (#${operator.lastQueuedId}) — awaiting the runner's own confirmation.`
                : "No commands sent from this console yet."}
        </p>
      </div>

      <OpenPositionsTable rows={openPositions} />

      <CommandAuditTrail entries={operator.auditLog} live={live} />

      <FillFeed fills={[...live.fills].reverse().slice(0, FILL_FEED_ROWS)} />

      {settledPositions.length > 0 && (
        <p className="method-note">
          {settledPositions.length} recently settled or unwound position(s) remain in
          the ledger below with their final numbers.
        </p>
      )}
    </section>
  );
}

/** An unsettled position has locked capital and no realized cents: the row
 * reports the fill it is holding, never a fabricated outcome. */
function OpenPositionsTable({ rows }: { rows: TradeRecord[] }) {
  if (rows.length === 0) {
    return (
      <p className="empty-inline">
        No open positions. Locked capital appears here while a hedge awaits
        settlement.
      </p>
    );
  }
  return (
    <div className="history-table-wrap" data-testid="open-positions">
      <table className="history-table trade-table">
        <caption className="method-note">
          Open positions — capital locked by the execution layer
        </caption>
        <thead>
          <tr>
            <th scope="col">#</th>
            <th scope="col">Market</th>
            <th scope="col">Venue orders</th>
            <th scope="col">Locked stake</th>
            <th scope="col">Settlement</th>
            <th scope="col">Realized</th>
          </tr>
        </thead>
        <tbody>
          {rows.map((record) => (
            <tr key={record.seq}>
              <th scope="row">{record.seq}</th>
              <td>{record.marketLabel}</td>
              <td>{formatVenueOrderIds(record)}</td>
              <td>{money(record.filledStakeCents ?? record.requestedStakeCents)}</td>
              <td>
                <span className="trade-badge trade-badge--open">{record.settlementStatus}</span>
              </td>
              <td>—</td>
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  );
}

/** Lifecycle of one audited command, derived only from what the wire proves:
 * a refusal is the worker's own answer; "in effect" means the runner's own
 * risk or session frames now match the command. Nothing here claims the
 * runner applied anything — the frames are the evidence, this is the
 * reading. A session-start stays "queued" because a matching start is
 * acknowledged as already-running with no distinct echo of its own. */
type AuditStatus = "refused" | "queued" | "in-effect";

const AUDIT_STATUS_LABELS: Record<AuditStatus, string> = {
  refused: "Refused",
  queued: "Queued",
  "in-effect": "In effect",
};

function auditStatus(entry: CommandAuditEntry, live: LiveSessionState): AuditStatus {
  if (entry.status === "refused") return "refused";
  switch (entry.command.t) {
    case "kill-switch":
      return live.risk !== null && live.risk.killSwitch === entry.command.engage
        ? "in-effect"
        : "queued";
    case "session-end":
      return live.sessionStatus === "ended" ? "in-effect" : "queued";
    case "session-start":
      return "queued";
  }
}

function auditActionLabel(command: OperatorCommand): string {
  switch (command.t) {
    case "kill-switch":
      return command.engage ? "Arm kill switch" : "Disarm kill switch";
    case "session-start":
      return `Start session (${command.mode})`;
    case "session-end":
      return "End session";
  }
}

function CommandAuditTrail({
  entries,
  live,
}: {
  entries: CommandAuditEntry[];
  live: LiveSessionState;
}) {
  return (
    <div className="command-audit" data-testid="command-audit">
      <h3>Command trail</h3>
      {entries.length === 0 ? (
        <p className="empty-inline">No commands sent from this console yet.</p>
      ) : (
        <>
          <p className="method-note">
            Commands this console sent, newest first. "In effect" reads the
            runner's own risk and session frames — queuing never implies
            application.
          </p>
          <ol className="command-audit-list">
            {entries.slice(0, AUDIT_VISIBLE_ROWS).map((entry, index) => {
              const status = auditStatus(entry, live);
              const sentAt = new Date(entry.sentAtMs);
              return (
                <li key={`${entry.sentAtMs}-${index}`}>
                  <span className="command-audit-action">
                    {auditActionLabel(entry.command)}
                  </span>
                  {entry.id !== null && (
                    <span className="command-audit-id">#{entry.id}</span>
                  )}
                  <time dateTime={sentAt.toISOString()}>
                    {sentAt.toLocaleTimeString()}
                  </time>
                  <span className={`trade-badge command-audit-status--${status}`}>
                    {AUDIT_STATUS_LABELS[status]}
                  </span>
                  {status === "refused" && entry.error !== undefined && (
                    <span className="command-audit-error">{entry.error}</span>
                  )}
                </li>
              );
            })}
          </ol>
        </>
      )}
    </div>
  );
}

function FillFeed({ fills }: { fills: FillRecord[] }) {
  return (
    <div className="fill-feed" data-testid="fill-feed">
      <h3>Fill reconciliation</h3>
      {fills.length === 0 ? (
        <p className="empty-inline">
          No reconciled fills reported yet. Events appear keyed by client/venue
          order ID as the ledger absorbs them.
        </p>
      ) : (
        <ul>
          {fills.map((fill) => (
            <li key={`${fill.clientOrderId}:${fill.venueOrderId ?? ""}`}>
              <code>{fill.clientOrderId}</code>
              {fill.venueOrderId !== null && <code> → {fill.venueOrderId}</code>}
              {" · "}
              {money(fill.filledStakeCents)} filled
              {" · "}
              {fill.settlementStatus}
              {fill.realizedProfitCents !== null && (
                <> · {money(fill.realizedProfitCents)} realized</>
              )}
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}

function formatVenueOrderIds(record: TradeRecord): string {
  if (record.venueOrderIds === undefined || record.venueOrderIds.length === 0) {
    return "—";
  }
  return record.venueOrderIds.join(", ");
}

/** `undefined` = no runner has spoken (unknown); `null` = a runner said it
 * enforces no cap. The two render differently — honesty about who said what. */
function formatCap(value: number | null | undefined): string {
  if (value === undefined) return "unknown";
  return value === null ? "not enforced" : money(value);
}

function formatPair(actual: number | null | undefined, max: number | null | undefined): string {
  if (actual === undefined) return "unknown";
  if (actual === null) return "not reported";
  return max === null || max === undefined ? `${actual}` : `${actual} of ${max}`;
}

function formatEdgeFloor(value: number | null | undefined): string {
  if (value === undefined) return "unknown";
  return value === null ? "not enforced" : `${value} bps`;
}

function formatLossBudget(risk: {
  maxDailyLossCents: number | null;
  dailyLossUsedCents: number | null;
}): string {
  if (risk.maxDailyLossCents === null) return "not enforced";
  const remaining = risk.maxDailyLossCents - (risk.dailyLossUsedCents ?? 0);
  return `${money(remaining)} remaining`;
}

function shortRunId(runId: string): string {
  return runId.split("-").slice(4).join("-") || runId;
}
