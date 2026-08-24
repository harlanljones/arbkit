//! Zod mirrors of the worker's viewer frames (`dashboard/worker/state.ts`).
//!
//! The display side of the frozen wire contract: every inbound frame is
//! parsed before it can touch React state, so a malformed push fails loudly
//! instead of rendering a recomputed or partial number. Money stays integer
//! cents; ratios stay integer bps; floats are rejected outright.

import { z } from "zod";
import { TradeRecordSchema } from "./schema";

const moneyCents = z.number().int();

export const TotalsSchema = z.object({
  trades: z.number().int(),
  stakedCents: moneyCents,
  theoreticalProfitCents: moneyCents,
  realizedProfitCents: moneyCents,
  expectedProfitCents: moneyCents,
  feesPaidCents: moneyCents,
  roiTheoreticalBps: z.number().int(),
  roiRealizedBps: z.number().int(),
});

export const FunnelSchema = z.object({
  attempted: z.number().int(),
  capitalShort: z.number().int(),
  clean: z.number().int(),
  proportional: z.number().int(),
  phantom: z.number().int(),
  brokenLeg: z.number().int(),
});

export const CapitalSchema = z.object({
  lockedCents: moneyCents.nullable(),
  availableCents: moneyCents.nullable(),
});

export const SessionHeaderSchema = z.object({
  runId: z.string().min(1),
  startedAtEpochMs: z.number().int(),
  initialBankrollCents: moneyCents.nullable(),
  ticksPerWindow: z.number().int().positive(),
  windowMs: z.number().int().positive(),
});

/** The runner's authoritative risk posture. A null cap means the runner
 * enforces none — displayed as "not enforced", never replaced with a
 * client-side number. */
export const RiskStateSchema = z.object({
  executionMode: z.enum(["paper", "live"]),
  killSwitch: z.boolean(),
  maxStakePerLegCents: moneyCents.nullable(),
  maxDailyLossCents: moneyCents.nullable(),
  dailyLossUsedCents: moneyCents.nullable(),
  maxOpenTrades: z.number().int().nullable(),
  openTrades: z.number().int().nullable(),
  minEdgeBps: z.number().int().nullable(),
});

/** One reconciled fill keyed by the execution layer's idempotency key. */
export const FillRecordSchema = z.object({
  clientOrderId: z.string().min(1),
  venueOrderId: z.string().min(1).nullable(),
  tradeSeq: z.number().int().nullable(),
  filledStakeCents: z.number().int(),
  realizedProfitCents: moneyCents.nullable(),
  settlementStatus: z.enum(["open", "settled", "unwound"]),
  reconciledAtEpochMs: z.number().int(),
});

const sessionStatusSchema = z.enum(["idle", "live", "stale", "ended"]);

export const ViewerFrameSchema = z.discriminatedUnion("t", [
  z.object({
    t: z.literal("hello"),
    serverTimeEpochMs: z.number().int(),
  }),
  z.object({
    t: z.literal("snapshot"),
    status: sessionStatusSchema,
    session: SessionHeaderSchema.nullable(),
    risk: RiskStateSchema.nullable(),
    fills: z.array(FillRecordSchema),
    totals: TotalsSchema,
    funnel: FunnelSchema,
    capital: CapitalSchema,
    windowsCompleted: z.number().int(),
    seqCursor: z.number().int(),
    items: z.array(TradeRecordSchema),
  }),
  z.object({
    t: z.literal("positions"),
    items: z.array(TradeRecordSchema),
  }),
  z.object({
    t: z.literal("totals"),
    status: sessionStatusSchema,
    risk: RiskStateSchema.nullable(),
    totals: TotalsSchema,
    funnel: FunnelSchema,
    capital: CapitalSchema,
    windowsCompleted: z.number().int(),
    seqCursor: z.number().int(),
  }),
  z.object({
    t: z.literal("risk"),
    state: RiskStateSchema,
  }),
  z.object({
    t: z.literal("fills"),
    items: z.array(FillRecordSchema),
  }),
]);

export type Totals = z.infer<typeof TotalsSchema>;
export type Funnel = z.infer<typeof FunnelSchema>;
export type Capital = z.infer<typeof CapitalSchema>;
export type SessionHeader = z.infer<typeof SessionHeaderSchema>;
export type RiskState = z.infer<typeof RiskStateSchema>;
export type FillRecord = z.infer<typeof FillRecordSchema>;
export type ViewerFrame = z.infer<typeof ViewerFrameSchema>;
