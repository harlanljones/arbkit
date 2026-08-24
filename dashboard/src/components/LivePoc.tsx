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

import { useMemo } from "react";
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
import type { TradeRecord } from "../data/schema";
import { useLiveSession } from "../data/useLiveSession";

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

export function LivePoc({ url }: { url?: string }) {
  const streamUrl = url ?? defaultLiveUrl();
  const live = useLiveSession(streamUrl);

  const recentRows = useMemo(
    () => live.items.slice(-VISIBLE_ROWS).reverse(),
    [live.items],
  );

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
            <dd>{formatLastFrame(live.lastFrameAtMs)}</dd>
          </div>
        </dl>
        <p className="live-console-proof">
          <span aria-hidden="true" /> Live data path · worker frames only · no simulated browser activity
        </p>
      </div>

      {/* A connected-but-idle room still pushes zeroed totals; only a real
          session header earns the numbers grid. */}
      {live.session === null || live.totals === null ? (
        <IdleNotice connecting={live.connection !== "open"} />
      ) : (
        <>
          <KpiGrid live={live} />

          <RoiSparkline series={live.roiSeries} />

          <RecentTable rows={recentRows} totalRows={live.items.length} />

          <p className="method-note">
            Paper trading on a synthetic workload: theoretical profit is the worst-case guarantee at
            lock time; realized profit reflects simulated fills after fees, slippage and queue decay.
            This is not live trading, and no orders are ever placed.
          </p>
        </>
      )}
    </div>
  );
}

function formatLastFrame(timestamp: number | null): string {
  if (timestamp === null) return "No frame received";
  const ageMs = Math.max(0, Date.now() - timestamp);
  if (ageMs < 1_000) return "<1s ago";
  if (ageMs < 60_000) return `${Math.floor(ageMs / 1_000)}s ago`;
  return `${Math.floor(ageMs / 60_000)}m ago`;
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
              <th scope="col">Worst case</th>
              <th scope="col">Realized</th>
              <th scope="col">Class</th>
            </tr>
          </thead>
          <tbody>
            {rows.map((record) => (
              <tr key={record.seq} className={record.realizedProfitCents > 0 ? undefined : "is-loss"}>
                <th scope="row">{record.seq}</th>
                <td>{record.marketLabel}</td>
                <td>{record.edgeBps} bps</td>
                <td>{money(record.requestedStakeCents)}</td>
                <td>{money(record.worstCaseProfitCents)}</td>
                <td>{money(record.realizedProfitCents)}</td>
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
