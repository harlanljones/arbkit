//! Reducer and wire-schema tests for the live session view.
//!
//! These pin the display contract: the client adopts authoritative server
//! numbers without recomputing them, resume replays never duplicate rows,
//! presentation history stays bounded, and a frame that fails schema
//! validation can be identified before it reaches state.

import { describe, expect, it } from "vitest";

import { ViewerFrameSchema } from "./liveSchema";
import {
  applyLiveFrame,
  initialLiveSession,
  MAX_ITEMS,
  MAX_ROI_POINTS,
} from "./liveSession";
import type { TradeRecord } from "./schema";

function record(seq: number, overrides: Partial<TradeRecord> = {}): TradeRecord {
  return {
    seq,
    detectionTimestampNs: 1_000 + seq,
    latencyNs: 5_000,
    marketLabel: "BOS @ LAL · moneyline",
    edgeBps: 459,
    overroundPpm: 956_100,
    requestedStakeCents: 100_000,
    expectedProfitCents: 4_583,
    worstCaseProfitCents: 4_583,
    realizedProfitCents: 4_583,
    slippageCents: 0,
    feesPaidCents: 3_661,
    fillRatioBps: 10_000,
    classification: "clean",
    chased: false,
    legs: [],
    ...overrides,
  };
}

const SNAPSHOT_BASE = {
  t: "snapshot" as const,
  status: "live" as const,
  session: {
    runId: "run-1",
    startedAtEpochMs: 1_000,
    initialBankrollCents: 1_000_000,
    ticksPerWindow: 200,
    windowMs: 1_000,
  },
  totals: {
    trades: 2,
    stakedCents: 200_000,
    theoreticalProfitCents: 9_166,
    realizedProfitCents: 9_166,
    expectedProfitCents: 9_166,
    feesPaidCents: 7_322,
    roiTheoreticalBps: 458,
    roiRealizedBps: 458,
  },
  funnel: {
    attempted: 2,
    capitalShort: 0,
    clean: 2,
    proportional: 0,
    phantom: 0,
    brokenLeg: 0,
  },
  capital: { lockedCents: 0, availableCents: 1_009_166 },
  windowsCompleted: 1,
  seqCursor: 1,
  items: [record(0), record(1)],
};

function snapshot(overrides: Partial<typeof SNAPSHOT_BASE> = {}) {
  return ViewerFrameSchema.parse({ ...SNAPSHOT_BASE, ...overrides });
}

function totalsFrame(
  overrides: Partial<Extract<ReturnType<typeof snapshot>, { t: "snapshot" }>["totals"]> = {},
) {
  const base = SNAPSHOT_BASE;
  return ViewerFrameSchema.parse({
    t: "totals",
    status: base.status,
    totals: { ...base.totals, trades: 3, ...overrides },
    funnel: base.funnel,
    capital: base.capital,
    windowsCompleted: base.windowsCompleted,
    seqCursor: 2,
  });
}

describe("applyLiveFrame", () => {
  it("adopts a snapshot wholesale as the authoritative state", () => {
    const next = applyLiveFrame(initialLiveSession, snapshot(), 5_000);
    expect(next.connection).toBe("open");
    expect(next.session?.runId).toBe("run-1");
    expect(next.totals?.trades).toBe(2);
    expect(next.seqCursor).toBe(1);
    expect(next.roiSeries).toHaveLength(1);
    expect(next.lastFrameAtMs).toBe(5_000);
  });

  it("appends only rows above the resume cursor", () => {
    let state = applyLiveFrame(initialLiveSession, snapshot(), 5_000);
    state = applyLiveFrame(
      state,
      ViewerFrameSchema.parse({
        t: "positions",
        items: [record(2), record(3)],
      }),
      5_100,
    );
    expect(state.items.map((r) => r.seq)).toEqual([0, 1, 2, 3]);

    // A resume replay of rows already applied changes nothing.
    const replayed = applyLiveFrame(
      state,
      ViewerFrameSchema.parse({ t: "positions", items: [record(0), record(3)] }),
      5_200,
    );
    expect(replayed).toBe(state);
  });

  it("caps the ledger at MAX_ITEMS newest rows", () => {
    let state = applyLiveFrame(initialLiveSession, snapshot(), 5_000);
    const flood = Array.from({ length: MAX_ITEMS + 50 }, (_, i) => record(i + 10));
    state = applyLiveFrame(state, ViewerFrameSchema.parse({ t: "positions", items: flood }), 6_000);
    expect(state.items).toHaveLength(MAX_ITEMS);
    expect(state.items[0].seq).toBe(60);
    expect(state.items.at(-1)?.seq).toBe(MAX_ITEMS + 59);
  });

  it("samples totals into the ROI series and skips identical repeats", () => {
    let state = applyLiveFrame(initialLiveSession, snapshot(), 5_000);
    state = applyLiveFrame(state, totalsFrame(), 6_000);
    expect(state.roiSeries).toHaveLength(2);
    expect(state.totals?.trades).toBe(3);

    // Same aggregates pushed again (heartbeat-adjacent): no extra sample.
    const repeated = applyLiveFrame(state, totalsFrame(), 6_100);
    expect(repeated.roiSeries).toHaveLength(2);

    // A changed total samples again.
    const advanced = applyLiveFrame(
      repeated,
      totalsFrame({ realizedProfitCents: 12_000, roiRealizedBps: 500 }),
      6_200,
    );
    expect(advanced.roiSeries).toHaveLength(3);
    expect(advanced.roiSeries.at(-1)).toEqual({
      atMs: 6_200,
      theoreticalBps: 458,
      realizedBps: 500,
    });
  });

  it("caps the ROI series", () => {
    let state = applyLiveFrame(initialLiveSession, snapshot(), 0);
    for (let i = 0; i < MAX_ROI_POINTS + 20; i += 1) {
      state = applyLiveFrame(
        state,
        totalsFrame({ trades: 3 + i, realizedProfitCents: 9_166 + i }),
        i * 250,
      );
    }
    expect(state.roiSeries).toHaveLength(MAX_ROI_POINTS);
  });

  it("tracks stale and ended transitions from totals frames", () => {
    let state = applyLiveFrame(initialLiveSession, snapshot(), 5_000);
    state = applyLiveFrame(
      state,
      ViewerFrameSchema.parse({
        t: "totals",
        status: "stale",
        totals: SNAPSHOT_BASE.totals,
        funnel: SNAPSHOT_BASE.funnel,
        capital: SNAPSHOT_BASE.capital,
        windowsCompleted: 1,
        seqCursor: 1,
      }),
      30_000,
    );
    expect(state.sessionStatus).toBe("stale");

    state = applyLiveFrame(
      state,
      ViewerFrameSchema.parse({
        t: "totals",
        status: "ended",
        totals: SNAPSHOT_BASE.totals,
        funnel: SNAPSHOT_BASE.funnel,
        capital: SNAPSHOT_BASE.capital,
        windowsCompleted: 3,
        seqCursor: 75,
      }),
      31_000,
    );
    expect(state.sessionStatus).toBe("ended");
  });

  it("rejects malformed viewer frames at the schema boundary", () => {
    expect(ViewerFrameSchema.safeParse({ t: "wat" }).success).toBe(false);
    expect(
      ViewerFrameSchema.safeParse({
        t: "totals",
        status: "live",
        totals: { ...SNAPSHOT_BASE.totals, stakedCents: 1.5 },
        funnel: SNAPSHOT_BASE.funnel,
        capital: SNAPSHOT_BASE.capital,
        windowsCompleted: 1,
        seqCursor: 1,
      }).success,
      "fractional cents must be rejected",
    ).toBe(false);
    expect(
      ViewerFrameSchema.safeParse({ t: "positions", items: [{ ...record(0), legs: "nope" }] })
        .success,
    ).toBe(false);
  });
});
