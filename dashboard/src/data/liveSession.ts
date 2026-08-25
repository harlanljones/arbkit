//! Pure frame reducer for the live session view.
//!
//! The WebSocket hook hands validated frames here; React state is produced
//! only by [`applyLiveFrame`], which keeps the arithmetic out of components
//! and the whole display model unit-testable without a socket. The server
//! owns every cumulative number — this reducer adopts, never recomputes.
//! The one thing it maintains locally is presentation history: a capped
//! ledger of recent rows and a capped ROI sampling series for the sparkline.

import type {
  Capital,
  FillRecord,
  Funnel,
  RiskState,
  SessionHeader,
  Totals,
  ViewerFrame,
} from "./liveSchema";
import type { TradeRecord } from "./schema";

/** Recent ledger rows kept for display; totals live server-side. */
export const MAX_ITEMS = 500;
/** ROI samples kept for the sparkline (~1 per totals push, burst-tolerant). */
export const MAX_ROI_POINTS = 720;
/** Recent fill-reconciliation events kept for the operator feed. */
export const MAX_FILLS = 128;

/** How long a supposedly-live session's stream may stay silent before the
 * page declares it stale on its own authority. Matches the worker's
 * `STALE_AFTER_MS` heartbeat budget by default: a runner beats every 5 s and
 * every ingest broadcasts totals, so four missed beats is generous. This is
 * the locally-observable complement to the server's verdict — it catches a
 * WebSocket that died without a close event, where no frame will ever arrive
 * to update anything. Page-level configurable via LivePoc's `silentAfterMs`. */
export const STREAM_SILENT_AFTER_MS = 20_000;

export type ConnectionStatus = "connecting" | "open" | "reconnecting";
export type SessionStatus = "idle" | "live" | "stale" | "ended";

export interface RoiPoint {
  atMs: number;
  theoreticalBps: number;
  realizedBps: number;
}

export interface LiveSessionState {
  connection: ConnectionStatus;
  session: SessionHeader | null;
  sessionStatus: SessionStatus;
  /** Authoritative risk posture, or null before any runner has reported —
   * null must be treated as kill-switch-engaged by every consumer. */
  risk: RiskState | null;
  fills: FillRecord[];
  totals: Totals | null;
  funnel: Funnel | null;
  capital: Capital | null;
  windowsCompleted: number;
  seqCursor: number;
  items: TradeRecord[];
  roiSeries: RoiPoint[];
  lastFrameAtMs: number | null;
}

export const initialLiveSession: LiveSessionState = {
  connection: "connecting",
  session: null,
  sessionStatus: "idle",
  risk: null,
  fills: [],
  totals: null,
  funnel: null,
  capital: null,
  windowsCompleted: 0,
  seqCursor: -1,
  items: [],
  roiSeries: [],
  lastFrameAtMs: null,
};

function capTail<T>(rows: T[], max: number): T[] {
  return rows.length > max ? rows.slice(rows.length - max) : rows;
}

export function applyLiveFrame(
  state: LiveSessionState,
  frame: ViewerFrame,
  nowMs: number,
): LiveSessionState {
  switch (frame.t) {
    case "hello":
      // Liveness proof only; aggregates ride on snapshot/totals frames.
      return { ...state, connection: "open", lastFrameAtMs: nowMs };

    case "snapshot":
      // Fully authoritative: adopt wholesale, including its ring window.
      return {
        ...state,
        connection: "open",
        session: frame.session,
        sessionStatus: frame.status,
        risk: frame.risk,
        fills: capTail(frame.fills, MAX_FILLS),
        totals: frame.totals,
        funnel: frame.funnel,
        capital: frame.capital,
        windowsCompleted: frame.windowsCompleted,
        seqCursor: Math.max(state.seqCursor, frame.seqCursor),
        items: capTail(frame.items, MAX_ITEMS),
        roiSeries: capTail(
          [
            ...state.roiSeries,
            {
              atMs: nowMs,
              theoreticalBps: frame.totals.roiTheoreticalBps,
              realizedBps: frame.totals.roiRealizedBps,
            },
          ],
          MAX_ROI_POINTS,
        ),
        lastFrameAtMs: nowMs,
      };

    case "positions": {
      // Rows arrive in ascending seq within a session; anything at or below
      // the cursor is a resume replay already on screen.
      const fresh = frame.items.filter((record) => record.seq > state.seqCursor);
      if (fresh.length === 0) return state;
      // Bare position batches advance the dedupe cursor too — totals frames
      // usually carry it, but the ledger must stay self-consistent even
      // between them.
      const nextCursor = fresh.reduce((max, record) => Math.max(max, record.seq), state.seqCursor);
      return {
        ...state,
        seqCursor: nextCursor,
        items: capTail([...state.items, ...fresh], MAX_ITEMS),
        lastFrameAtMs: nowMs,
      };
    }

    case "totals": {
      const nextCursor = Math.max(state.seqCursor, frame.seqCursor);
      if (
        state.totals !== null &&
        state.sessionStatus === frame.status &&
        state.seqCursor === nextCursor &&
        shallowTotalsEqual(state.totals, frame.totals)
      ) {
        // Identical repeat push (heartbeat-adjacent): keep history sparse.
        return { ...state, lastFrameAtMs: nowMs };
      }
      return {
        ...state,
        sessionStatus: frame.status,
        risk: frame.risk,
        totals: frame.totals,
        funnel: frame.funnel,
        capital: frame.capital,
        windowsCompleted: frame.windowsCompleted,
        seqCursor: nextCursor,
        roiSeries: capTail(
          [
            ...state.roiSeries,
            {
              atMs: nowMs,
              theoreticalBps: frame.totals.roiTheoreticalBps,
              realizedBps: frame.totals.roiRealizedBps,
            },
          ],
          MAX_ROI_POINTS,
        ),
        lastFrameAtMs: nowMs,
      };
    }

    case "risk":
      // The runner's own posture statement, adopted verbatim — never merged
      // with what we held before, so a runner restart cannot inherit a
      // stale disarm.
      return { ...state, risk: frame.state, lastFrameAtMs: nowMs };

    case "fills": {
      const merged = dedupeFills([...state.fills, ...frame.items]);
      return { ...state, fills: capTail(merged, MAX_FILLS), lastFrameAtMs: nowMs };
    }
  }
}

/** Fill identity is the reconciliation key: client order id plus venue order
 * id once assigned. A re-pushed fill replaces its earlier self (at-least-once
 * delivery), keeping the newest report for each order. */
function dedupeFills(fills: FillRecord[]): FillRecord[] {
  const byKey = new Map<string, FillRecord>();
  for (const fill of fills) {
    byKey.set(`${fill.clientOrderId}:${fill.venueOrderId ?? ""}`, fill);
  }
  return [...byKey.values()];
}

function shallowTotalsEqual(a: Totals, b: Totals): boolean {
  return (
    a.trades === b.trades &&
    a.stakedCents === b.stakedCents &&
    a.theoreticalProfitCents === b.theoreticalProfitCents &&
    a.realizedProfitCents === b.realizedProfitCents
  );
}
