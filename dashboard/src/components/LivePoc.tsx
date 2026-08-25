//! LivePoc: the streaming proof-of-concept view.
//!
//! Renders whatever the [`useLiveSession`] hook has adopted from the
//! authoritative worker frames — KPI cards for theoretical-vs-realized ROI,
//! the disposition funnel, an ROI sparkline, and the most recent ledger rows.
//! Like TradeLedger, nothing displayed is recomputed in JSX: every number is
//! an integer produced by the worker's session arithmetic and formatted, not
//! derived.
//!
//! Honesty rules carry over: no session says so plainly, a stale runner says
//! so, and theoretical profit is always labeled as the worst-case guarantee
//! at lock time — never confused with realized settlement.

import { useEffect, useMemo, useState } from "react";
import {
  CartesianGrid,
  Line,
  LineChart,
  ReferenceLine,
  ResponsiveContainer,
  Tooltip,
  XAxis,
  YAxis,
} from "recharts";
import { money, percent } from "../data/metrics";
import type { RiskState } from "../data/liveSchema";
import { STREAM_SILENT_AFTER_MS } from "../data/liveSession";
import type { TradeRecord } from "../data/schema";
import { useLiveSession } from "../data/useLiveSession";
import { useOperator } from "../data/useOperator";
import { OperatorConsole } from "./OperatorConsole";

const GREEN = "#2f6d43";
const COPPER = "#9a4d2f";

const CONNECTION_LABELS = {
  connecting: "Connecting…",
  open: "Stream connected",
  reconnecting: "Reconnecting…",
} as const;

const SESSION_LABELS = {
  idle: "Waiting for a runner",
  live: "Session live",
  stale: "Runner silent — session stale",
  ended: "Session ended",
} as const;

/** Rows shown in the recent-ledger table, newest first. */
const VISIBLE_ROWS = 40;

function defaultLiveUrl(): string {
  if (typeof window === "undefined") return "/api/live/ws";
  const scheme = window.location.protocol === "https:" ? "wss" : "ws";
  return `${scheme}://${window.location.host}/api/live/ws`;
}

/** Re-renders its consumer on a fixed cadence so time-derived labels (frame
 * age, stream silence) keep moving while nothing else changes. Without this,
 * a page showing "last frame 2s ago" would freeze there forever the moment
 * frames stop arriving — stale data dressed as fresh. */
function useNowTick(intervalMs = 1_000): number {
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    const timer = setInterval(() => setNow(Date.now()), intervalMs);
    return () => clearInterval(timer);
  }, [intervalMs]);
  return now;
}

export function LivePoc({
  url,
  silentAfterMs = STREAM_SILENT_AFTER_MS,
}: {
  url?: string;
  /** Stream-silence budget for a supposedly-live session. Defaults to
   * `STREAM_SILENT_AFTER_MS`, matching the worker's heartbeat verdict. */
  silentAfterMs?: number;
}) {
  const streamUrl = url ?? defaultLiveUrl();
  const live = useLiveSession(streamUrl);
  const operator = useOperator();
  const now = useNowTick();

  const recentRows = useMemo(
    () => live.items.slice(-VISIBLE_ROWS).reverse(),
    [live.items],
  );
  const containsLiveExecution = live.items.some((item) => item.executionMode === "live");

  return (
    <div className="trade-ledger live-poc">
      <div className="live-console-head">
        <div className="live-console-title">
          <strong>Worker frame intake</strong>
          <span>Validated, authoritative data from the running session</span>
        </div>
        <div className="live-statusbar" role="status" aria-live="polite">
          <span className={`live-pill live-pill--${live.connection}`}>
            {CONNECTION_LABELS[live.connection]}
          </span>
          <span className={`live-pill live-pill--session-${live.sessionStatus}`}>
            {SESSION_LABELS[live.sessionStatus]}
          </span>
        </div>
        <dl className="live-console-meta">
          <div>
            <dt>Stream endpoint</dt>
            <dd>{streamUrl}</dd>
          </div>
          <div>
            <dt>Session / run</dt>
            <dd>{live.session ? shortRunId(live.session.runId) : "Awaiting session header"}</dd>
          </div>
          <div>
            <dt>Sequence cursor</dt>
            <dd>{live.seqCursor >= 0 ? live.seqCursor : "—"}</dd>
          </div>
          <div>
            <dt>Last worker frame</dt>
            <dd>{formatLastFrame(live.lastFrameAtMs, now)}</dd>
          </div>
        </dl>
        <p className="live-console-proof">
          <span aria-hidden="true" /> Live data path · worker frames only · no simulated browser activity
        </p>
      </div>

      <StreamHealthBanner
        connection={live.connection}
        sessionStatus={live.sessionStatus}
        lastFrameAtMs={live.lastFrameAtMs}
        now={now}
        silentAfterMs={silentAfterMs}
      />

      {/* A connected-but-idle room still pushes zeroed totals; only a real
          session header earns the numbers grid. The operator console is the
          one surface that renders even while idle: posture is always worth
          showing, and it fails inert without a connection. */}
      <OperatorConsole live={live} operator={operator} />

      {live.session === null || live.totals === null ? (
        <IdleNotice connecting={live.connection !== "open"} />
      ) : (
        <>
          <KpiGrid live={live} />

          <SessionHealthPanel live={live} />

          <RoiSparkline series={live.roiSeries} />

          <RecentTable rows={recentRows} totalRows={live.items.length} />

          <p className="method-note">
            {containsLiveExecution
              ? "Live Trading: real capital, not synthetic. Realized cents and settlement status come from the execution service."
              : "Paper trading on a synthetic workload: theoretical profit is the worst-case guarantee at lock time; realized profit reflects simulated fills after fees, slippage and queue decay. No orders are placed."}
          </p>
        </>
      )}
    </div>
  );
}

/** An unsettled live trade has no realized profit yet: rendering `$0.00`
 * would fabricate a settlement that never happened, so the wire's
 * `settlementStatus` is shown instead and the row is not styled as a loss. */
function renderRealized(record: TradeRecord): string {
  if (record.realizedProfitCents !== null) return money(record.realizedProfitCents);
  if (record.settlementStatus === "open") return "Open";
  if (record.settlementStatus === "unwound") return "Unwound";
  return "—";
}

function realizedRowDisposition(record: TradeRecord): string | undefined {
  if (record.realizedProfitCents === null) return undefined;
  return record.realizedProfitCents > 0 ? undefined : "is-loss";
}

function formatLastFrame(timestamp: number | null, now: number): string {
  if (timestamp === null) return "No frame received";
  const ageMs = Math.max(0, now - timestamp);
  return `${formatAge(ageMs)} ago`;
}

/** Human age for health labels: "<1s", "12s", "3m 20s". */
function formatAge(ageMs: number): string {
  if (ageMs < 1_000) return "<1s";
  if (ageMs < 60_000) return `${Math.floor(ageMs / 1_000)}s`;
  const minutes = Math.floor(ageMs / 60_000);
  const seconds = Math.floor((ageMs % 60_000) / 1_000);
  return seconds > 0 ? `${minutes}m ${seconds}s` : `${minutes}m`;
}

/** The three locally-observable stream states, kept distinct on purpose:
 *
 * - **disconnected** — no socket (connecting or reconnecting). Controls are
 *   inert and every number below is last-known.
 * - **stale** — the socket is open, a session claims to be live, yet no
 *   frame has crossed for longer than the silence budget. This is the case
 *   the server cannot report through a dead pipe: data above is frozen as
 *   of its last frame and must be read as outdated.
 * - **healthy** — frames are flowing; includes an idle room waiting for a
 *   runner and an ended session, where silence is expected and is not
 *   staleness. */
type StreamHealth = "disconnected" | "stale" | "healthy";

function streamHealth(
  connection: string,
  sessionStatus: string,
  lastFrameAtMs: number | null,
  now: number,
  silentAfterMs: number,
): { health: StreamHealth; ageMs: number | null } {
  if (connection !== "open") return { health: "disconnected", ageMs: null };
  const ageMs = lastFrameAtMs === null ? null : Math.max(0, now - lastFrameAtMs);
  const silent =
    sessionStatus === "live" &&
    ageMs !== null &&
    ageMs >= silentAfterMs;
  return silent ? { health: "stale", ageMs } : { health: "healthy", ageMs };
}

const STREAM_HEALTH_LABELS: Record<StreamHealth, string> = {
  disconnected: "Stream disconnected",
  stale: "Stream silent",
  healthy: "Stream live",
};

function StreamHealthBanner({
  connection,
  sessionStatus,
  lastFrameAtMs,
  now,
  silentAfterMs,
}: {
  connection: string;
  sessionStatus: string;
  lastFrameAtMs: number | null;
  now: number;
  silentAfterMs: number;
}) {
  const { health, ageMs } = streamHealth(
    connection,
    sessionStatus,
    lastFrameAtMs,
    now,
    silentAfterMs,
  );

  let detail: string;
  if (health === "disconnected") {
    detail =
      connection === "connecting"
        ? "Opening the live stream…"
        : "Reconnecting — everything below is last-known data.";
  } else if (health === "stale") {
    detail = `no frame for ${formatAge(ageMs ?? 0)} — data below is frozen as of its last frame; treat it as outdated until frames resume.`;
  } else if (sessionStatus === "idle") {
    detail = "waiting for a runner — silence here is expected.";
  } else {
    detail =
      ageMs === null
        ? "awaiting first frame."
        : `last frame ${formatAge(ageMs)} ago.`;
  }

  return (
    <div
      className={`stream-health stream-health--${health}`}
      role="status"
      aria-live="polite"
      data-testid="stream-health"
    >
      <span className={`live-pill live-pill--stream-${health}`}>
        {STREAM_HEALTH_LABELS[health]}
      </span>
      <span className="stream-health-detail">{detail}</span>
    </div>
  );
}

function shortRunId(runId: string): string {
  // Drop the epoch prefix; keep platform and commit so provenance survives.
  return runId.split("-").slice(4).join("-") || runId;
}

function IdleNotice({ connecting }: { connecting: boolean }) {
  return (
    <div>
      <p className="empty-inline">
        {connecting
          ? "Opening the live stream…"
          : "No live session is running right now."}
      </p>
      <p className="method-note">
        Start one from the repository root:{" "}
        <code>
          cargo run -p arbkit-engine --example live_runner -- --token-env ARBLIVE_TOKEN
        </code>{" "}
        (add <code>--url</code> to point it somewhere other than localhost).
        Detected-and-settled paper positions will appear here as the runner
        streams them.
      </p>
    </div>
  );
}

type LiveView = ReturnType<typeof useLiveSession>;

function KpiGrid({ live }: { live: LiveView }) {
  const { totals, funnel, capital, windowsCompleted } = live;
  if (totals === null || funnel === null) return null;

  return (
    <>
      <div className="financial-ledger" role="group" aria-label="Live session financial summary">
        <div>
          <span>Theoretical profit</span>
          <strong>{money(totals.theoreticalProfitCents)}</strong>
          <small>{percent(totals.roiTheoreticalBps)} · worst case at lock</small>
        </div>
        <div>
          <span>Realized profit</span>
          <strong>{money(totals.realizedProfitCents)}</strong>
          <small>{percent(totals.roiRealizedBps)} settled ROI</small>
        </div>
        <div>
          <span>Total staked</span>
          <strong>{money(totals.stakedCents)}</strong>
          <small>{totals.trades} trades · {windowsCompleted} windows</small>
        </div>
        <div>
          <span>Venue fees paid</span>
          <strong>{money(totals.feesPaidCents)}</strong>
          <small>
            capital{" "}
            {capital?.lockedCents === null || capital === null
              ? "—"
              : `${money(capital.lockedCents)} locked`}
          </small>
        </div>
      </div>

      <div className="live-funnel" role="group" aria-label="Disposition funnel">
        <span className="trade-chip is-active">Attempted {funnel.attempted}</span>
        <span className="trade-chip">Capital-short {funnel.capitalShort}</span>
        <span className="trade-chip is-active">Clean {funnel.clean}</span>
        <span className="trade-chip is-active">Proportional {funnel.proportional}</span>
        <span className="trade-chip">Phantom {funnel.phantom}</span>
        <span className="trade-chip">Broken leg {funnel.brokenLeg}</span>
      </div>
    </>
  );
}

/** Micro-live session health: the runner's own execution counters and the
 * caps in force, adopted verbatim. Counters a live runner has not yet
 * reported render as "—" (fails inert, like the rest of the page); zeros it
 * has reported render as data. The phantom rate is derived from the funnel's
 * authoritative counts and shown against the paper baseline (10.01%, the
 * deterministic sim phantom rate) the micro-live halt rule compares against. */
function SessionHealthPanel({ live }: { live: LiveView }) {
  const { funnel, risk } = live;
  if (funnel === null) return null;

  const phantomRateBps =
    funnel.attempted > 0
      ? Math.round((funnel.phantom / funnel.attempted) * 10_000)
      : null;

  return (
    <section className="session-health" role="group" aria-label="Micro-live session health">
      <div className="session-health-head">
        <strong>Session health</strong>
        <span>Micro-live counters and caps, straight from the runner</span>
      </div>
      <dl className="live-console-meta session-health-meta">
        <div>
          <dt>Attempted</dt>
          <dd>{funnel.attempted}</dd>
        </div>
        <div>
          <dt>Unwind failures</dt>
          <dd>{funnel.unwindFailures ?? "—"}</dd>
        </div>
        <div>
          <dt>Ack matched</dt>
          <dd>{funnel.ackMatched ?? "—"}</dd>
        </div>
        <div>
          <dt>In-flight remaining</dt>
          <dd>{funnel.inFlightRemaining ?? "—"}</dd>
        </div>
        <div>
          <dt>Phantom rate</dt>
          <dd>
            {phantomRateBps === null
              ? "—"
              : `${percent(phantomRateBps)} of ${funnel.attempted} attempted`}
            <small className="session-health-note">paper baseline 10.01%</small>
          </dd>
        </div>
        <div>
          <dt>Per-leg cap</dt>
          <dd>{renderCapCents(risk, risk?.maxStakePerLegCents ?? null)}</dd>
        </div>
        <div>
          <dt>Daily loss cap</dt>
          <dd>{renderCapCents(risk, risk?.maxDailyLossCents ?? null)}</dd>
        </div>
        <div>
          <dt>Open-trade cap</dt>
          <dd>{renderCapCount(risk, risk?.maxOpenTrades ?? null)}</dd>
        </div>
      </dl>
    </section>
  );
}

/** A null cap is the runner's own "not enforced"; a missing runner report is
 * "—" (we do not know the posture, so we claim none). */
function renderCapCents(risk: RiskState | null, cents: number | null): string {
  if (risk === null) return "—";
  return cents === null ? "Not enforced" : money(cents);
}

function renderCapCount(risk: RiskState | null, count: number | null): string {
  if (risk === null) return "—";
  return count === null ? "Not enforced" : String(count);
}

function RoiSparkline({
  series,
}: {
  series: { atMs: number; theoreticalBps: number; realizedBps: number }[];
}) {
  if (series.length < 2) return null;

  return (
    <figure className="chart-figure">
      <div className="chart-frame chart-frame--live">
        <ResponsiveContainer width="100%" height="100%">
          <LineChart data={series} margin={{ top: 12, right: 20, bottom: 4, left: 4 }} accessibilityLayer>
            <CartesianGrid stroke="#d7d0c0" strokeDasharray="2 5" />
            <XAxis dataKey="atMs" hide />
            <YAxis
              tickFormatter={(value) => `${Number(value).toFixed(1)}%`}
              width={64}
              tickLine={false}
              axisLine={{ stroke: "#171814" }}
              domain={["auto", "auto"]}
            />
            <Tooltip
              formatter={(value, name) => [
                percent(Number(value)),
                name === "realizedBps" ? "Realized" : "Theoretical",
              ]}
              labelFormatter={() => ""}
              contentStyle={{
                background: "#f3efe5",
                border: "1px solid #171814",
                borderRadius: 0,
                fontFamily: "IBM Plex Mono, monospace",
                fontSize: 12,
              }}
            />
            {/* Zero line: below it the session is losing money outright. */}
            <ReferenceLine y={0} stroke={COPPER} strokeDasharray="4 4" />
            <Line
              type="monotone"
              dataKey="theoreticalBps"
              stroke={COPPER}
              strokeDasharray="5 4"
              strokeWidth={1.5}
              dot={false}
              isAnimationActive={false}
            />
            <Line
              type="monotone"
              dataKey="realizedBps"
              stroke={GREEN}
              strokeWidth={2}
              dot={false}
              isAnimationActive={false}
            />
          </LineChart>
        </ResponsiveContainer>
      </div>
      <figcaption>
        Session ROI since you opened this page. The dashed copper line is the worst-case edge at
        detection; the solid green line is what simulated settlement actually kept.
      </figcaption>
    </figure>
  );
}

function RecentTable({
  rows,
  totalRows,
}: {
  rows: TradeRecord[];
  totalRows: number;
}) {
  if (rows.length === 0) {
    return (
      <p className="empty-inline">
        No positions have been locked yet. The runner streams each one as its
        simulation settles.
      </p>
    );
  }

  return (
    <div>
      <p className="method-note trade-count" aria-live="polite">
        Showing the {rows.length} most recent of {totalRows} streamed positions (newest first).
      </p>
      <div className="history-table-wrap">
        <table className="history-table trade-table">
          <thead>
            <tr>
              <th scope="col">#</th>
              <th scope="col">Market</th>
              <th scope="col">Edge</th>
              <th scope="col">Stake</th>
              <th scope="col">Theoretical</th>
              <th scope="col">Realized</th>
              <th scope="col">Class</th>
            </tr>
          </thead>
          <tbody>
            {rows.map((record) => (
              <tr
                key={record.seq}
                className={realizedRowDisposition(record)}
              >
                <th scope="row">{record.seq}</th>
                <td>{record.marketLabel}</td>
                <td>{record.edgeBps} bps</td>
                <td>{money(record.requestedStakeCents)}</td>
                <td>{money(record.worstCaseProfitCents)}</td>
                <td>{renderRealized(record)}</td>
                <td>
                  <span
                    className={`trade-badge trade-badge--${record.classification}`}
                    data-classification={record.classification}
                  >
                    {record.classification}
                  </span>
                  {record.chased && <span className="trade-badge trade-badge--chased">Chased</span>}
                </td>
              </tr>
            ))}
          </tbody>
        </table>
      </div>
    </div>
  );
}
