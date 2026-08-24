//! The authoritative live-session state, as one pure class.
//!
//! The Durable Object owns this instance; the browser never sums anything.
//! Every cumulative number (staked, theoretical profit, realized profit,
//! funnel counts) is computed here exactly once and pushed to viewers, so a
//! late joiner gets correct totals from a snapshot without replaying history
//! — the ring only holds the recent ledger rows, never the arithmetic.
//!
//! All money stays integer cents; ROI ratios are floored toward negative
//! infinity (`Math.floor`), matching the Rust runner's `div_euclid` habit:
//! a reported ratio rounds down even when negative. Every number the page
//! shows should be one you can beat, not one you must hit.

import type { RunnerFrame, TradeRecord } from "./wire";

export { LIVE_SCHEMA_VERSION } from "./wire";
export type { TradeRecord } from "./wire";

/** A session with no heartbeat for this long reads as stale. The runner
 * beats every 5 s, so four missed beats is generous. */
export const STALE_AFTER_MS = 20_000;
/** Recent trade rows retained for snapshots and resume-by-seq. */
export const RING_CAPACITY = 256;

export type SessionStatus = "idle" | "live" | "stale" | "ended";

export interface SessionHeader {
  runId: string;
  startedAtEpochMs: number;
  initialBankrollCents: number | null;
  ticksPerWindow: number;
  windowMs: number;
}

export interface Totals {
  trades: number;
  stakedCents: number;
  theoreticalProfitCents: number;
  realizedProfitCents: number;
  expectedProfitCents: number;
  feesPaidCents: number;
  roiTheoreticalBps: number;
  roiRealizedBps: number;
}

export interface Funnel {
  attempted: number;
  capitalShort: number;
  clean: number;
  proportional: number;
  phantom: number;
  brokenLeg: number;
}

export interface Capital {
  lockedCents: number | null;
  availableCents: number | null;
}

/** ROI in integer bps, floored toward negative infinity. */
function roiBps(profitCents: number, stakedCents: number): number {
  if (stakedCents <= 0) return 0;
  return Math.floor((profitCents * 10_000) / stakedCents);
}

export class PositionSession {
  private header: SessionHeader | null = null;
  private status: SessionStatus = "idle";
  private lastActivityAtMs = 0;

  private trades = 0;
  private stakedCents = 0;
  private theoreticalProfitCents = 0;
  private realizedProfitCents = 0;
  private expectedProfitCents = 0;
  private feesPaidCents = 0;

  private funnelCounts: Funnel = {
    attempted: 0,
    capitalShort: 0,
    clean: 0,
    proportional: 0,
    phantom: 0,
    brokenLeg: 0,
  };

  private capital: Capital = { lockedCents: null, availableCents: null };
  private windowsCompleted = 0;
  private seqCursor = -1;

  /** Newest-at-the-end ring of recent records. */
  private ring: TradeRecord[] = [];

  getStatus(): SessionStatus {
    return this.status;
  }

  getLastActivityAtMs(): number {
    return this.lastActivityAtMs;
  }

  getSeqCursor(): number {
    return this.seqCursor;
  }

  /** Applies one already-validated runner frame. `now` is server wall clock:
   * liveness is measured by the clock that judges it, not the runner's. */
  apply(frame: RunnerFrame, now: number): void {
    switch (frame.t) {
      case "session-start": {
        this.reset();
        this.header = {
          runId: frame.runId,
          startedAtEpochMs: frame.startedAtEpochMs,
          initialBankrollCents: frame.initialBankrollCents,
          ticksPerWindow: frame.ticksPerWindow,
          windowMs: frame.windowMs,
        };
        this.status = "live";
        break;
      }
      case "positions": {
        // Ingest retries are at-least-once: a request may have reached the
        // Durable Object even when the runner did not receive its response.
        // Runner sequence numbers are monotonic within a session, so rows at
        // or below the cursor are already accounted for.
        const fresh: TradeRecord[] = [];
        let nextSeq = this.seqCursor;
        for (const record of frame.items) {
          if (record.seq <= nextSeq) continue;
          fresh.push(record);
          nextSeq = record.seq;
        }
        for (const record of fresh) {
          this.trades += 1;
          this.stakedCents += record.requestedStakeCents;
          this.theoreticalProfitCents += record.worstCaseProfitCents;
          this.realizedProfitCents += record.realizedProfitCents;
          this.expectedProfitCents += record.expectedProfitCents;
          this.feesPaidCents += record.feesPaidCents;
          this.funnelCounts[record.classification] += 1;
          this.seqCursor = Math.max(this.seqCursor, record.seq);
        }
        this.pushRing(fresh);
        break;
      }
      case "stats": {
        this.capital = {
          lockedCents: frame.lockedCents,
          availableCents: frame.availableCents,
        };
        this.funnelCounts.attempted = frame.attempted;
        this.funnelCounts.capitalShort = frame.capitalShort;
        this.windowsCompleted = Math.max(this.windowsCompleted, frame.windowsCompleted);
        this.seqCursor = Math.max(this.seqCursor, frame.seqCursor);
        break;
      }
      case "heartbeat": {
        // Heartbeats exist so silence is measurable; nothing else to record.
        break;
      }
      case "session-end": {
        this.status = "ended";
        break;
      }
    }
    if (this.status !== "ended") this.status = "live";
    this.lastActivityAtMs = now;
  }

  /** Flips an active session to stale once its heartbeat budget expires.
   * Returns true exactly when a transition happened (so the DO broadcasts). */
  markStaleIfExpired(now: number): boolean {
    if (this.status !== "live") return false;
    if (now - this.lastActivityAtMs < STALE_AFTER_MS) return false;
    this.status = "stale";
    return true;
  }

  /** Authoritative cumulative numbers, recomputed on demand — cheap integer
   * math, and impossible for any viewer to hold a divergent copy of. */
  totals(): Totals {
    return {
      trades: this.trades,
      stakedCents: this.stakedCents,
      theoreticalProfitCents: this.theoreticalProfitCents,
      realizedProfitCents: this.realizedProfitCents,
      expectedProfitCents: this.expectedProfitCents,
      feesPaidCents: this.feesPaidCents,
      roiTheoreticalBps: roiBps(this.theoreticalProfitCents, this.stakedCents),
      roiRealizedBps: roiBps(this.realizedProfitCents, this.stakedCents),
    };
  }

  funnel(): Readonly<Funnel> {
    return this.funnelCounts;
  }

  currentCapital(): Readonly<Capital> {
    return this.capital;
  }

  currentHeader(): SessionHeader | null {
    return this.header;
  }

  windows(): number {
    return this.windowsCompleted;
  }

  /** Ring slice after `afterSeq`, oldest first. `afterSeq` below the oldest
   * retained row yields everything still held — the honest answer for a very
   * late joiner is "recent history", not a lie of completeness. */
  recordsAfter(afterSeq: number): TradeRecord[] {
    return this.ring.filter((record) => record.seq > afterSeq);
  }

  private pushRing(items: TradeRecord[]): void {
    this.ring.push(...items);
    if (this.ring.length > RING_CAPACITY) {
      this.ring.splice(0, this.ring.length - RING_CAPACITY);
    }
  }

  private reset(): void {
    this.header = null;
    this.trades = 0;
    this.stakedCents = 0;
    this.theoreticalProfitCents = 0;
    this.realizedProfitCents = 0;
    this.expectedProfitCents = 0;
    this.feesPaidCents = 0;
    this.funnelCounts = {
      attempted: 0,
      capitalShort: 0,
      clean: 0,
      proportional: 0,
      phantom: 0,
      brokenLeg: 0,
    };
    this.capital = { lockedCents: null, availableCents: null };
    this.windowsCompleted = 0;
    this.seqCursor = -1;
    this.ring = [];
    this.status = "idle";
    this.lastActivityAtMs = 0;
  }
}

// ---------------------------------------------------------------------------
// Viewer-facing frames. Plain objects, JSON-ready; the DO stringifies once
// per broadcast. The browser-side zod mirrors live in src/data/liveSchema.ts.
// ---------------------------------------------------------------------------

export function helloFrame(serverTimeEpochMs: number) {
  return { t: "hello", serverTimeEpochMs } as const;
}

export function snapshotFrame(session: PositionSession, afterSeq: number) {
  return {
    t: "snapshot",
    status: session.getStatus(),
    session: session.currentHeader(),
    totals: session.totals(),
    funnel: session.funnel(),
    capital: session.currentCapital(),
    windowsCompleted: session.windows(),
    seqCursor: session.getSeqCursor(),
    items: session.recordsAfter(afterSeq),
  } as const;
}

export function totalsFrame(session: PositionSession) {
  return {
    t: "totals",
    status: session.getStatus(),
    totals: session.totals(),
    funnel: session.funnel(),
    capital: session.currentCapital(),
    windowsCompleted: session.windows(),
    seqCursor: session.getSeqCursor(),
  } as const;
}
