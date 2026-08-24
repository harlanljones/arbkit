//! Zod mirrors of the runner's live-frame protocol (`frames.rs` in
//! `arbkit-engine/examples/live_runner`).
//!
//! These duplicate — deliberately — the shapes in `src/data/schema.ts`: the
//! dashboard validates what it renders, the worker validates what it ingests,
//! and both mirror the same frozen Rust serialization. Money stays integer
//! cents and rates stay integer bps/ppm; floats are rejected outright so no
//! display path can silently recompute a pessimistic number.

import { z } from "zod";

/** Schema version carried by every `session-start` frame. */
export const LIVE_SCHEMA_VERSION = 1 as const;

const moneyCents = z.number().int();

const legStatusSchema = z.union([
  z.literal("filled"),
  z.object({
    partiallyFilled: z.object({
      filledCents: z.number().int(),
      unfilledCents: z.number().int(),
      reason: z.string().min(1),
    }),
  }),
  z.object({
    unfilled: z.string().min(1),
  }),
]);

const tradeLegSchema = z.object({
  venueLabel: z.string().min(1),
  outcomeLabel: z.string().min(1),
  status: legStatusSchema,
  requestedStakeCents: moneyCents,
  filledStakeCents: z.number().int(),
  netPayoutCents: moneyCents,
});

export const tradeRecordSchema = z.object({
  seq: z.number().int(),
  detectionTimestampNs: z.number().int(),
  latencyNs: z.number().int(),
  marketLabel: z.string().min(1),
  edgeBps: z.number().int(),
  overroundPpm: z.number().int(),
  requestedStakeCents: moneyCents,
  expectedProfitCents: moneyCents,
  worstCaseProfitCents: moneyCents,
  realizedProfitCents: moneyCents,
  slippageCents: moneyCents,
  feesPaidCents: z.number().int(),
  fillRatioBps: z.number().int(),
  classification: z.enum(["clean", "proportional", "phantom", "brokenLeg"]),
  chased: z.boolean(),
  legs: z.array(tradeLegSchema).max(4),
});

export type TradeRecord = z.infer<typeof tradeRecordSchema>;

export const runnerFrameSchema = z.discriminatedUnion("t", [
  z.object({
    t: z.literal("session-start"),
    schemaVersion: z.literal(LIVE_SCHEMA_VERSION),
    runId: z.string().min(1),
    startedAtEpochMs: z.number().int(),
    initialBankrollCents: moneyCents.nullable(),
    ticksPerWindow: z.number().int().positive(),
    windowMs: z.number().int().positive(),
  }),
  z.object({
    t: z.literal("positions"),
    items: z.array(tradeRecordSchema).max(256),
  }),
  z.object({
    t: z.literal("stats"),
    seqCursor: z.number().int(),
    windowsCompleted: z.number().int(),
    lockedCents: moneyCents.nullable(),
    availableCents: moneyCents.nullable(),
    attempted: z.number().int(),
    capitalShort: z.number().int(),
  }),
  z.object({
    t: z.literal("heartbeat"),
    seqCursor: z.number().int(),
  }),
  z.object({
    t: z.literal("session-end"),
  }),
]);

export type RunnerFrame = z.infer<typeof runnerFrameSchema>;
