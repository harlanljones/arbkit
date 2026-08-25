//! Unit tests for the worker's authoritative session state.
//!
//! These pin the arithmetic the whole POC rests on: cumulative totals are
//! computed exactly once here, ROI floors toward negative infinity, the ring
//! caps and resumes by seq, staleness transitions fire exactly once, and a
//! malformed frame can never poison already-accumulated totals.

import { describe, expect, it } from "vitest";

import {
  PositionSession,
  RING_CAPACITY,
  STALE_AFTER_MS,
  snapshotFrame,
  totalsFrame,
} from "./state";
import { runnerFrameSchema, type TradeRecord } from "./wire";

function record(seq: number, overrides: Partial<TradeRecord> = {}): TradeRecord {
  return {
    seq,
    detectionTimestampNs: 1_000 + seq,
    latencyNs: 5_000,
    marketLabel: "BOS @ LAL · moneyline",
    edgeBps: 200,
    overroundPpm: 980_000,
    requestedStakeCents: 100_000,
    expectedProfitCents: 2_000,
    worstCaseProfitCents: 2_000,
    realizedProfitCents: 2_000,
    slippageCents: 0,
    feesPaidCents: 350,
    fillRatioBps: 10_000,
    classification: "clean",
    chased: false,
    legs: [],
    ...overrides,
  };
}

function positions(items: TradeRecord[]) {
  return runnerFrameSchema.parse({ t: "positions", items });
}

function sessionStart(runId = "run-1") {
  return runnerFrameSchema.parse({
    t: "session-start",
    schemaVersion: 1,
    runId,
    startedAtEpochMs: 1_000,
    initialBankrollCents: 1_000_000,
    ticksPerWindow: 200,
    windowMs: 1_000,
  });
}

describe("PositionSession totals", () => {
  it("accumulates staked, theoretical and realized across batches", () => {
    const session = new PositionSession();
    session.apply(sessionStart(), 10);
    session.apply(
      positions([
        record(0, { requestedStakeCents: 90_000, worstCaseProfitCents: 1_800, realizedProfitCents: 1_800 }),
        record(1, { requestedStakeCents: 95_389, worstCaseProfitCents: 4_379, realizedProfitCents: 4_379 }),
      ]),
      11,
    );
    session.apply(
      positions([record(2, { requestedStakeCents: 100_000, worstCaseProfitCents: 4_583, realizedProfitCents: -1_200 })]),
      12,
    );

    const totals = session.totals();
    expect(totals.trades).toBe(3);
    expect(totals.stakedCents).toBe(285_389);
    expect(totals.theoreticalProfitCents).toBe(10_762);
    expect(totals.realizedProfitCents).toBe(4_979);
    // Floor, not round: a negative-leaning ratio never rounds itself up.
    expect(totals.roiTheoreticalBps).toBe(Math.floor((10_762 * 10_000) / 285_389));
    expect(totals.roiRealizedBps).toBe(Math.floor((4_979 * 10_000) / 285_389));
  });

  it("does not count an at-least-once ingest retry twice", () => {
    const session = new PositionSession();
    session.apply(sessionStart(), 10);
    const batch = positions([
      record(0, { requestedStakeCents: 90_000 }),
      record(1, { requestedStakeCents: 95_000 }),
    ]);

    session.apply(batch, 11);
    session.apply(batch, 12);

    expect(session.totals()).toMatchObject({
      trades: 2,
      stakedCents: 185_000,
      realizedProfitCents: 4_000,
    });
    expect(session.recordsAfter(-1).map((item) => item.seq)).toEqual([0, 1]);
  });

  it("floors negative ROI toward negative infinity", () => {
    const session = new PositionSession();
    session.apply(sessionStart(), 10);
    // -1.5 bps: floor must land at -2; any rounding would flatter it to -1.
    session.apply(
      positions([record(0, { requestedStakeCents: 100_000, realizedProfitCents: -15 })]),
      11,
    );
    expect(session.totals().roiRealizedBps).toBe(-2);
  });

  it("counts the disposition funnel from records", () => {
    const session = new PositionSession();
    session.apply(sessionStart(), 10);
    session.apply(
      positions([
        record(0),
        record(1, { classification: "phantom", realizedProfitCents: 0 }),
        record(2, { classification: "brokenLeg", realizedProfitCents: -500 }),
        record(3, { classification: "proportional" }),
      ]),
      11,
    );
    expect(session.funnel()).toMatchObject({
      clean: 1,
      phantom: 1,
      brokenLeg: 1,
      proportional: 1,
      attempted: 0, // funnel attempted comes only from stats frames
      capitalShort: 0,
    });
  });

  it("adopts capital and attempted/capital-short from stats frames", () => {
    const session = new PositionSession();
    session.apply(sessionStart(), 10);
    session.apply(
      positions([record(0)]),
      11,
    );
    session.apply(
      runnerFrameSchema.parse({
        t: "stats",
        seqCursor: 41,
        windowsCompleted: 3,
        lockedCents: 99_999,
        availableCents: 900_001,
        attempted: 44,
        capitalShort: 3,
        unwindFailures: 1,
        ackMatched: 40,
        inFlightRemaining: 2,
      }),
      12,
    );
    expect(session.currentCapital()).toEqual({ lockedCents: 99_999, availableCents: 900_001 });
    expect(session.funnel()).toMatchObject({
      attempted: 44,
      capitalShort: 3,
      unwindFailures: 1,
      ackMatched: 40,
      inFlightRemaining: 2,
    });
    expect(session.windows()).toBe(3);
    expect(session.getSeqCursor()).toBe(41);
  });

  it("resets everything on a new session-start", () => {
    const session = new PositionSession();
    session.apply(sessionStart("old"), 10);
    session.apply(positions([record(0)]), 11);
    session.apply(sessionStart("new"), 20);

    expect(session.totals()).toMatchObject({ trades: 0, stakedCents: 0 });
    expect(session.getSeqCursor()).toBe(-1);
    expect(session.recordsAfter(0)).toHaveLength(0);
    expect(snapshotFrame(session, 0).session?.runId).toBe("new");
  });

  it("returns to live after stale once activity resumes", () => {
    const session = new PositionSession();
    session.apply(sessionStart(), 1_000);
    expect(session.markStaleIfExpired(1_000 + STALE_AFTER_MS + 1)).toBe(true);
    expect(session.getStatus()).toBe("stale");
    expect(session.markStaleIfExpired(2_000 + STALE_AFTER_MS)).toBe(false);

    session.apply(runnerFrameSchema.parse({ t: "heartbeat", seqCursor: 0 }), 3_000);
    expect(session.getStatus()).toBe("live");
  });

  it("never marks an ended session stale", () => {
    const session = new PositionSession();
    session.apply(sessionStart(), 1_000);
    session.apply(runnerFrameSchema.parse({ t: "session-end" }), 1_500);
    expect(session.getStatus()).toBe("ended");
    expect(session.markStaleIfExpired(999_999)).toBe(false);
  });
});

describe("ring and resume semantics", () => {
  it("caps the ring at RING_CAPACITY newest records", () => {
    const session = new PositionSession();
    session.apply(sessionStart(), 10);
    // Two batches: a single frame is capped at 256 items on the wire, so
    // overflow must be reachable across batches too.
    const all = Array.from({ length: RING_CAPACITY + 40 }, (_, i) => record(i));
    session.apply(positions(all.slice(0, 100)), 11);
    session.apply(positions(all.slice(100)), 12);

    const kept = session.recordsAfter(-1);
    expect(kept).toHaveLength(RING_CAPACITY);
    expect(kept[0].seq).toBe(40); // oldest evicted
    expect(kept.at(-1)?.seq).toBe(RING_CAPACITY + 39);
  });

  it("serves resume slices strictly above afterSeq", () => {
    const session = new PositionSession();
    session.apply(sessionStart(), 10);
    session.apply(positions([record(7), record(8), record(9)]), 11);

    expect(session.recordsAfter(8).map((r) => r.seq)).toEqual([9]);
    // A cursor older than everything retained still yields recent history.
    expect(session.recordsAfter(-5)).toHaveLength(3);
    expect(snapshotFrame(session, 8).items.map((r) => r.seq)).toEqual([9]);
  });

  it("emits viewer frames with authoritative aggregates attached", () => {
    const session = new PositionSession();
    session.apply(sessionStart(), 10);
    session.apply(positions([record(3)]), 11);

    const totals = totalsFrame(session);
    expect(totals.t).toBe("totals");
    expect(totals.totals.trades).toBe(1);
    expect(totals.status).toBe("live");

    const snap = snapshotFrame(session, 0);
    expect(snap.items).toHaveLength(1);
    expect(snap.session?.initialBankrollCents).toBe(1_000_000);
  });
});

describe("wire validation", () => {
  it("rejects malformed frames instead of guessing", () => {
    expect(runnerFrameSchema.safeParse({ t: "wat" }).success).toBe(false);
    expect(
      runnerFrameSchema.safeParse({
        t: "positions",
        items: [{ ...record(0), requestedStakeCents: 1.5 }],
      }).success,
      "fractional cents must be rejected",
    ).toBe(false);
    expect(
      runnerFrameSchema.safeParse({
        t: "session-start",
        schemaVersion: 2,
        runId: "x",
        startedAtEpochMs: 1,
        initialBankrollCents: null,
        ticksPerWindow: 1,
        windowMs: 1,
      }).success,
      "wrong schema version must be rejected",
    ).toBe(false);
  });

  it("accepts the live-execution extensions and rejects unknown settlements", () => {
    // LIVE_TRADING.md's optional fields ride on an otherwise paper record;
    // a nullable realizedProfitCents is what makes "open" expressible.
    const live = record(0, {
      executionMode: "live",
      venueOrderIds: ["kalshi-1", "poly-0xabc"],
      filledStakeCents: 95_389,
      settlementStatus: "open",
      realizedProfitCents: null,
    });
    expect(runnerFrameSchema.safeParse({ t: "positions", items: [live] }).success).toBe(true);

    for (const settlementStatus of ["promised", "OPEN", null]) {
      expect(
        runnerFrameSchema.safeParse({
          t: "positions",
          items: [record(1, { settlementStatus })],
        }).success,
        `settlementStatus ${String(settlementStatus)} must be rejected`,
      ).toBe(false);
    }
  });

  it("accepts the exact frames the Rust runner emits", () => {
    // Byte-for-byte shapes observed from a live_runner smoke session.
    expect(
      runnerFrameSchema.safeParse({
        t: "session-start",
        schemaVersion: 1,
        runId: "1787530482650-linux-x86_64-2bd8baa-live",
        startedAtEpochMs: 1787530482650,
        initialBankrollCents: 10000000,
        ticksPerWindow: 200,
        windowMs: 400,
      }).success,
    ).toBe(true);
    expect(
      runnerFrameSchema.safeParse({
        t: "positions",
        items: [
          {
            seq: 37,
            detectionTimestampNs: 400095679,
            latencyNs: 1571403,
            marketLabel: "Boston Celtics @ Los Angeles Lakers · moneyline",
            edgeBps: 459,
            overroundPpm: 956100,
            requestedStakeCents: 95389,
            expectedProfitCents: 4379,
            worstCaseProfitCents: 4379,
            realizedProfitCents: 4379,
            slippageCents: 0,
            feesPaidCents: 3492,
            fillRatioBps: 10000,
            classification: "clean",
            chased: false,
            legs: [
              {
                venueLabel: "polymarket",
                outcomeLabel: "Los Angeles Lakers Moneyline",
                status: "filled",
                requestedStakeCents: 47889,
                filledStakeCents: 47889,
                netPayoutCents: 99768,
              },
              {
                venueLabel: "kalshi",
                outcomeLabel: "Boston Celtics Moneyline",
                status: { unfilled: "priceMoved" },
                requestedStakeCents: 47500,
                filledStakeCents: 0,
                netPayoutCents: 0,
              },
            ],
          },
        ],
      }).success,
    ).toBe(true);
  });
});
