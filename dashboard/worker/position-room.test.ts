//! Ingest-path tests for the `PositionRoom` Durable Object.
//!
//! These drive the real `fetch`/`alarm` handlers over stubbed storage and
//! stubbed/injected sockets — no Cloudflare runtime, no module mocks.
//! Pinned here: size- and schema-guarded ingest, at-least-once retry
//! idempotency, session restart fan-out, the ended-session gate, the
//! staleness alarm lifecycle, operational stats surfacing, and the viewer
//! hello/snapshot/resume handshake.

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { PositionRoom } from "./position-room";
import { STALE_AFTER_MS } from "./state";
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

function wireFrame(frameData: Record<string, unknown>): string {
  return JSON.stringify(runnerFrameSchema.parse(frameData));
}

function sessionStartFrame(runId: string): string {
  return wireFrame({
    t: "session-start",
    schemaVersion: 1,
    runId,
    startedAtEpochMs: 1_000,
    initialBankrollCents: 1_000_000,
    ticksPerWindow: 200,
    windowMs: 1_000,
  });
}

function positionsFrame(items: TradeRecord[]): string {
  return wireFrame({ t: "positions", items });
}

type Ctx = ConstructorParameters<typeof PositionRoom>[0];
type Env = ConstructorParameters<typeof PositionRoom>[1];

/** Minimal stand-in for DO storage: only the alarm surface is touched. */
class FakeAlarmStorage {
  alarmAt: number | null = null;
  async getAlarm(): Promise<number | null> {
    return this.alarmAt;
  }
  async setAlarm(at: number): Promise<void> {
    this.alarmAt = at;
  }
}

/** Server side of a faked WebSocketPair: records everything sent to it. */
class FakeServerSocket {
  readonly sent: string[] = [];
  private readonly handlers = new Map<string, ((event: MessageEvent) => void)[]>();

  accept(): void {}
  send(data: string): void {
    this.sent.push(data);
  }
  addEventListener(type: string, handler: (event: MessageEvent) => void): void {
    this.handlers.set(type, [...(this.handlers.get(type) ?? []), handler]);
  }
  message(data: unknown): void {
    const event = new MessageEvent("message", { data: JSON.stringify(data) });
    for (const handler of this.handlers.get("message") ?? []) handler(event);
  }
  frames(): Record<string, unknown>[] {
    return this.sent.map((line) => JSON.parse(line) as Record<string, unknown>);
  }
}

function makeRoom(): {
  room: PositionRoom;
  storage: FakeAlarmStorage;
  connectViewer(): FakeServerSocket;
} {
  const storage = new FakeAlarmStorage();
  const ctx = { storage } as unknown as Ctx;
  const room = new PositionRoom(ctx, {} as Env);
  return {
    room,
    storage,
    connectViewer() {
      const socket = new FakeServerSocket();
      (room as unknown as { viewers: Set<object> }).viewers.add(socket);
      return socket;
    },
  };
}

async function ingest(room: PositionRoom, body: string): Promise<Response> {
  return room.fetch(
    new Request("https://room.internal/api/live/ingest", { method: "POST", body }),
  );
}

/** Opens a real viewer connection through handleViewer with WebSocketPair
 * stubbed, returning the server-side socket that received the frames. */
async function connectOverHttp(room: PositionRoom): Promise<FakeServerSocket> {
  const servers: FakeServerSocket[] = [];
  vi.stubGlobal(
    "WebSocketPair",
    class {
      // Cloudflare's pair is index-addressable: `pair[0]` is the client.
      [0]: { close(): void } = { close() {} };
      [1] = new FakeServerSocket();
      constructor() {
        servers.push(this[1]);
      }
    },
  );
  try {
    await room.fetch(
      new Request("https://room.internal/api/live/ws", { headers: { upgrade: "websocket" } }),
    ).catch((error: unknown) => {
      // Node's Response refuses the workers-only 101 status; handleViewer
      // has already accepted the socket and sent its greeting by then, so
      // exactly this error is the success path here.
      expect(String(error)).toContain("status");
    });
    expect(servers).toHaveLength(1);
    return servers[0]!;
  } finally {
    vi.unstubAllGlobals();
  }
}

describe("PositionRoom ingest", () => {
  it("relays a valid batch as snapshot/positions/totals and answers 204", async () => {
    const { room } = makeRoom();
    const viewer = await connectOverHttp(room);

    const response = await ingest(
      room,
      [sessionStartFrame("run-1"), positionsFrame([record(0), record(1)])].join("\n"),
    );

    expect(response.status).toBe(204);
    const pushed = viewer.frames();
    // A viewer attached to the idle room gets an honest empty snapshot
    // first. At the session boundary the room sends one authoritative
    // snapshot instead of a separate positions push — the rows ride in its
    // items, already paired with the header they belong to.
    expect(pushed.map((f) => f.t)).toEqual(["hello", "snapshot", "snapshot", "totals"]);
    expect(pushed[1].status).toBe("idle");
    expect((pushed[2].session as Record<string, unknown>).runId).toBe("run-1");
    expect((pushed[2].items as { seq: number }[]).map((item) => item.seq)).toEqual([0, 1]);
    expect(pushed[3]).toMatchObject({ t: "totals", status: "live" });
    expect(pushed[3].totals).toMatchObject({ trades: 2 });
  });

  it("counts an at-least-once retry of a whole batch exactly once", async () => {
    const { room } = makeRoom();
    const batch = [sessionStartFrame("run-1"), positionsFrame([record(0), record(1)])].join("\n");

    await ingest(room, batch);
    expect((await ingest(room, batch)).status).toBe(204);

    const viewer = await connectOverHttp(room);
    const snapshot = viewer.frames().at(-1)!;
    expect(snapshot.totals).toMatchObject({
      trades: 2,
      stakedCents: 200_000,
      realizedProfitCents: 4_000,
    });
    expect(snapshot.items).toHaveLength(2);
  });

  it("rejects frames after session-end until a new session-start", async () => {
    const { room } = makeRoom();
    await ingest(room, [sessionStartFrame("run-1"), wireFrame({ t: "session-end" })].join("\n"));

    const rejected = await ingest(room, positionsFrame([record(0)]));
    expect(rejected.status).toBe(409);
    expect(await rejected.json()).toMatchObject({
      error: expect.stringContaining("session already ended"),
    });

    expect((await ingest(room, sessionStartFrame("run-2"))).status).toBe(204);
  });

  it("answers 400 for invalid JSON and schema-invalid records", async () => {
    const { room } = makeRoom();

    const badJson = await ingest(room, `${sessionStartFrame("run-1")}\n{not json}`);
    expect(badJson.status).toBe(400);
    expect(await badJson.json()).toMatchObject({ error: "line 2 is not valid JSON" });

    const badSchema = await ingest(
      room,
      `${sessionStartFrame("run-1")}\n${JSON.stringify({
        t: "positions",
        items: [{ ...record(0), requestedStakeCents: 1.5 }],
      })}`,
    );
    expect(badSchema.status).toBe(400);
    expect(await badSchema.json()).toMatchObject({
      error: expect.stringContaining("failed schema validation"),
    });
  });

  it("refuses oversized batches before parsing", async () => {
    const { room } = makeRoom();
    const response = await ingest(room, "x".repeat(512 * 1024 + 1));
    expect(response.status).toBe(413);
  });

  it("answers 405 for non-POST ingest requests", async () => {
    const { room } = makeRoom();
    const response = await room.fetch(new Request("https://room.internal/api/live/ingest"));
    expect(response.status).toBe(405);
  });

  it("surfaces runner stats as operational metrics on every totals push", async () => {
    const { room } = makeRoom();
    const viewer = connectViewerSync(room);

    await ingest(
      room,
      [
        sessionStartFrame("run-1"),
        wireFrame({
          t: "stats",
          seqCursor: 41,
          windowsCompleted: 3,
          lockedCents: 99_999,
          availableCents: 900_001,
          attempted: 44,
          capitalShort: 3,
        }),
      ].join("\n"),
    );

    const totalsPush = viewer.frames().at(-1)!;
    expect(totalsPush.windowsCompleted).toBe(3);
    expect(totalsPush.seqCursor).toBe(41);
    expect(totalsPush.capital).toEqual({ lockedCents: 99_999, availableCents: 900_001 });
    expect(totalsPush.funnel).toMatchObject({ attempted: 44, capitalShort: 3 });
  });
});

describe("PositionRoom staleness", () => {
  beforeEach(() => {
    vi.useFakeTimers();
  });
  afterEach(() => {
    vi.useRealTimers();
  });

  it("arms the alarm after activity and keeps an earlier one while fresh", async () => {
    vi.setSystemTime(1_000);
    const { room, storage } = makeRoom();
    await ingest(room, sessionStartFrame("run-1"));
    expect(storage.alarmAt).toBe(11_000);

    // A later beat must not push the check further out than the existing one.
    vi.setSystemTime(5_000);
    await ingest(room, wireFrame({ t: "heartbeat", seqCursor: 0 }));
    expect(storage.alarmAt).toBe(11_000);
  });

  it("flips to stale once, broadcasts it, and lets alarms die out", async () => {
    vi.setSystemTime(1_000);
    const { room, storage } = makeRoom();
    const viewer = connectViewerSync(room);
    await ingest(room, sessionStartFrame("run-1"));
    expect(viewer.sent).toHaveLength(2); // snapshot + totals

    // The runtime consumes an alarm before invoking it; model that here.
    storage.alarmAt = null;
    vi.setSystemTime(20_999);
    await room.alarm();
    expect(viewer.sent).toHaveLength(2);
    // Still live: the check must keep coming.
    expect(storage.alarmAt).toBe(30_999);

    storage.alarmAt = null;
    vi.setSystemTime(21_001);
    await room.alarm();
    expect(viewer.frames().at(-1)).toMatchObject({ t: "totals", status: "stale" });
    // A stale session is idle until its runner returns: no more alarms.
    expect(storage.alarmAt).toBeNull();
  });

  it("returns to live and re-arms when the runner comes back", async () => {
    vi.setSystemTime(1_000);
    const { room, storage } = makeRoom();
    const viewer = connectViewerSync(room);
    await ingest(room, sessionStartFrame("run-1"));

    storage.alarmAt = null;
    vi.setSystemTime(21_001);
    await room.alarm();
    expect(viewer.sent).toHaveLength(3);

    vi.setSystemTime(30_000);
    await ingest(room, wireFrame({ t: "heartbeat", seqCursor: 7 }));
    expect(storage.alarmAt).toBe(40_000);
    expect(viewer.frames().at(-1)).toMatchObject({ t: "totals", status: "live" });
  });

  it("leaves an idle room alone when the alarm fires", async () => {
    vi.setSystemTime(1_000);
    const { room, storage } = makeRoom();
    const viewer = connectViewerSync(room);

    await room.alarm();
    expect(storage.alarmAt).toBeNull();
    expect(viewer.sent).toHaveLength(0);
  });
});

describe("PositionRoom viewer handshake", () => {
  it("greets with hello plus snapshot and serves resume slices by cursor", async () => {
    const { room } = makeRoom();
    await ingest(
      room,
      [sessionStartFrame("run-1"), positionsFrame([record(7), record(8), record(9)])].join("\n"),
    );

    const server = await connectOverHttp(room);
    const greeted = server.frames();
    expect(greeted.map((f) => f.t)).toEqual(["hello", "snapshot"]);
    expect(greeted[1].seqCursor).toBe(9);
    expect(greeted[1].items).toHaveLength(3);

    server.message({ t: "resume", afterSeq: 8 });
    const sliced = server.frames().at(-1)!;
    expect(sliced.t).toBe("snapshot");
    expect((sliced.items as { seq: number }[]).map((item) => item.seq)).toEqual([9]);

    // A cursor older than everything retained yields recent history.
    server.message({ t: "resume", afterSeq: -1 });
    const replayed = server.frames().at(-1)!;
    expect(replayed.items).toHaveLength(3);
  });

  it("ignores unparseable viewer chatter instead of failing", async () => {
    const { room } = makeRoom();
    const server = await connectOverHttp(room);
    const before = server.sent.length;

    expect(() =>
      server.message({ t: "resume", afterSeq: "not a number" }),
    ).not.toThrow();
    expect(server.sent).toHaveLength(before);
  });

  it("answers non-websocket requests with 426", async () => {
    const { room } = makeRoom();
    const response = await room.fetch(new Request("https://room.internal/api/live/ws"));
    expect(response.status).toBe(426);
  });
});

/** Injects a fake socket directly into the private viewer set: same fan-out
 * surface as a real connection, minus the upgrade handshake. */
function connectViewerSync(room: PositionRoom): FakeServerSocket {
  const socket = new FakeServerSocket();
  (room as unknown as { viewers: Set<object> }).viewers.add(socket);
  return socket;
}
