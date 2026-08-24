//! Component smoke test for the live view over a stubbed WebSocket.
//!
//! Drives the real production path — hook opens the socket, frames are
//! schema-validated and buffered, the 250 ms flush commits them to React,
//! and the KPI grid renders the authoritative integers — plus the resume
//! handshake on reconnect. No module mocks beyond the socket itself.

import { act, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { LivePoc } from "./LivePoc";

class MockSocket {
  static OPEN = 1;
  static instances: MockSocket[] = [];

  sent: string[] = [];
  readyState = 0;
  private handlers = new Map<string, ((event: Event) => void)[]>();

  constructor(public readonly url: string) {
    MockSocket.instances.push(this);
  }

  addEventListener(type: string, handler: (event: Event) => void): void {
    const list = this.handlers.get(type) ?? [];
    list.push(handler);
    this.handlers.set(type, list);
  }

  send(data: string): void {
    this.sent.push(data);
  }

  close(): void {
    this.readyState = 3;
    this.emit("close");
  }

  open(): void {
    this.readyState = 1;
    this.emit("open");
  }

  message(data: unknown): void {
    this.emit(new MessageEvent("message", { data: JSON.stringify(data) }));
  }

  private emit(typeOrEvent: string | Event): void {
    const event =
      typeof typeOrEvent === "string" ? new Event(typeOrEvent) : typeOrEvent;
    for (const handler of this.handlers.get(event.type) ?? []) handler(event);
  }
}

const SNAPSHOT_FRAME = {
  t: "snapshot",
  status: "live",
  session: {
    runId: "1787530482650-linux-x86_64-2bd8baa-live",
    startedAtEpochMs: 1_000,
    initialBankrollCents: 10_000_000,
    ticksPerWindow: 200,
    windowMs: 400,
  },
  totals: {
    trades: 76,
    stakedCents: 7_433_794,
    theoreticalProfitCents: 227_915,
    realizedProfitCents: 204_780,
    expectedProfitCents: 227_915,
    feesPaidCents: 240_015,
    roiTheoreticalBps: 306,
    roiRealizedBps: 275,
  },
  funnel: {
    attempted: 103,
    capitalShort: 27,
    clean: 68,
    proportional: 0,
    phantom: 8,
    brokenLeg: 0,
  },
  capital: { lockedCents: 0, availableCents: 10_204_780 },
  windowsCompleted: 3,
  seqCursor: 75,
  items: [
    {
      seq: 75,
      detectionTimestampNs: 800188630,
      latencyNs: 1658669,
      marketLabel: "Boston Celtics @ Los Angeles Lakers · moneyline",
      edgeBps: 458,
      overroundPpm: 956100,
      requestedStakeCents: 100000,
      expectedProfitCents: 4583,
      worstCaseProfitCents: 4583,
      realizedProfitCents: 4583,
      slippageCents: 0,
      feesPaidCents: 3661,
      fillRatioBps: 10000,
      classification: "clean",
      chased: false,
      legs: [],
    },
  ],
};

describe("LivePoc", () => {
  beforeEach(() => {
    vi.useFakeTimers();
    MockSocket.instances = [];
    vi.stubGlobal("WebSocket", MockSocket as unknown as typeof WebSocket);
  });

  afterEach(() => {
    vi.unstubAllGlobals();
    vi.useRealTimers();
  });

  function lastSocket(): MockSocket {
    return MockSocket.instances.at(-1) as MockSocket;
  }

  it("renders authoritative totals from streamed frames", async () => {
    render(<LivePoc url="ws://test/api/live/ws" />);
    await act(async () => {
      await vi.advanceTimersByTimeAsync(0);
    });
    const socket = lastSocket();
    socket.open();
    socket.message({ t: "hello", serverTimeEpochMs: 1 });

    // Before the flush interval elapses, nothing is committed yet.
    expect(screen.queryByText("$227.91")).toBeNull();

    socket.message(SNAPSHOT_FRAME);
    await act(async () => {
      await vi.advanceTimersByTimeAsync(250);
    });

    expect(screen.getByText("Session live")).toBeInTheDocument();
    expect(screen.getByText("$2,279.15")).toBeInTheDocument(); // theoretical
    expect(screen.getByText("$2,047.80")).toBeInTheDocument(); // realized
    expect(screen.getByText("3.06% · worst case at lock")).toBeInTheDocument();
    expect(screen.getByText("Attempted 103")).toBeInTheDocument();
    expect(screen.getByText("76 trades · 3 windows")).toBeInTheDocument();
    expect(screen.getByRole("columnheader", { name: "Theoretical" })).toBeInTheDocument();
    expect(screen.queryByRole("columnheader", { name: "Worst case" })).toBeNull();

    // A live position push lands as its own ledger row on the next tick.
    socket.message({
      t: "positions",
      items: [{ ...SNAPSHOT_FRAME.items[0], seq: 76 }],
    });
    await act(async () => {
      await vi.advanceTimersByTimeAsync(250);
    });
    expect(
      screen.getByText(/most recent of 2 streamed positions \(newest first\)\./),
    ).toBeInTheDocument();
  });

  it("resumes by cursor within the same session on reconnect", async () => {
    render(<LivePoc url="ws://test/api/live/ws" />);
    await act(async () => {
      await vi.advanceTimersByTimeAsync(0);
    });

    const first = lastSocket();
    first.open();
    first.message(SNAPSHOT_FRAME);
    await act(async () => {
      await vi.advanceTimersByTimeAsync(250);
    });

    first.close();
    await act(async () => {
      // Past the maximum jittered backoff window (500 ms base).
      await vi.advanceTimersByTimeAsync(2_000);
    });

    const second = lastSocket();
    expect(second).not.toBe(first);
    second.open();
    // The resume request is scoped to the session that earned the cursor.
    expect(second.sent).toContain(JSON.stringify({ t: "resume", afterSeq: 75 }));

    // An idle room answers honestly instead of inventing a session.
    second.message({
      t: "snapshot",
      status: "idle",
      session: null,
      totals: {
        trades: 0,
        stakedCents: 0,
        theoreticalProfitCents: 0,
        realizedProfitCents: 0,
        expectedProfitCents: 0,
        feesPaidCents: 0,
        roiTheoreticalBps: 0,
        roiRealizedBps: 0,
      },
      funnel: {
        attempted: 0,
        capitalShort: 0,
        clean: 0,
        proportional: 0,
        phantom: 0,
        brokenLeg: 0,
      },
      capital: { lockedCents: null, availableCents: null },
      windowsCompleted: 0,
      seqCursor: -1,
      items: [],
    });
    await act(async () => {
      await vi.advanceTimersByTimeAsync(250);
    });
    expect(screen.getByText("Waiting for a runner")).toBeInTheDocument();
    expect(
      screen.getByText(/No live session is running right now\./),
    ).toBeInTheDocument();
  });

  it("labels a paper session as synthetic and never shows the live banner", async () => {
    render(<LivePoc url="ws://test/api/live/ws" />);
    await act(async () => {
      await vi.advanceTimersByTimeAsync(0);
    });
    const socket = lastSocket();
    socket.open();
    socket.message(SNAPSHOT_FRAME);
    await act(async () => {
      await vi.advanceTimersByTimeAsync(250);
    });

    expect(screen.getByText(/Paper trading on a synthetic workload/)).toBeInTheDocument();
    expect(screen.queryByText(/Live Trading: real capital/)).toBeNull();
  });

  it("raises the live-capital banner when a live execution record streams in", async () => {
    render(<LivePoc url="ws://test/api/live/ws" />);
    await act(async () => {
      await vi.advanceTimersByTimeAsync(0);
    });
    const socket = lastSocket();
    socket.open();
    socket.message({
      ...SNAPSHOT_FRAME,
      items: [
        {
          ...SNAPSHOT_FRAME.items[0],
          seq: 75,
          executionMode: "live",
          settlementStatus: "settled",
          venueOrderIds: ["kalshi-1", "poly-0xabc"],
        },
      ],
    });
    await act(async () => {
      await vi.advanceTimersByTimeAsync(250);
    });

    expect(screen.getByText(/Live Trading: real capital, not synthetic/)).toBeInTheDocument();
    expect(screen.queryByText(/Paper trading on a synthetic workload/)).toBeNull();
  });

  it("renders open and unwound settlements explicitly instead of $0.00", async () => {
    render(<LivePoc url="ws://test/api/live/ws" />);
    await act(async () => {
      await vi.advanceTimersByTimeAsync(0);
    });
    const socket = lastSocket();
    socket.open();
    socket.message({
      ...SNAPSHOT_FRAME,
      items: [
        {
          ...SNAPSHOT_FRAME.items[0],
          seq: 74,
          executionMode: "live",
          settlementStatus: "unwound",
          realizedProfitCents: null,
        },
        {
          ...SNAPSHOT_FRAME.items[0],
          seq: 75,
          executionMode: "live",
          settlementStatus: "open",
          realizedProfitCents: null,
        },
      ],
    });
    await act(async () => {
      await vi.advanceTimersByTimeAsync(250);
    });

    expect(screen.getByText("Open")).toBeInTheDocument();
    expect(screen.getByText("Unwound")).toBeInTheDocument();
    expect(screen.queryByText("$0.00", { selector: "td" })).toBeNull();
    // An unsettled trade is not a loss; only known non-positive outcomes are.
    expect(document.querySelectorAll("tr.is-loss")).toHaveLength(0);
  });

  it("switches the session pill to stale when the runner goes silent", async () => {
    render(<LivePoc url="ws://test/api/live/ws" />);
    await act(async () => {
      await vi.advanceTimersByTimeAsync(0);
    });
    const socket = lastSocket();
    socket.open();
    socket.message(SNAPSHOT_FRAME);
    await act(async () => {
      await vi.advanceTimersByTimeAsync(250);
    });
    expect(screen.getByText("Session live")).toBeInTheDocument();

    socket.message({
      t: "totals",
      status: "stale",
      totals: SNAPSHOT_FRAME.totals,
      funnel: SNAPSHOT_FRAME.funnel,
      capital: SNAPSHOT_FRAME.capital,
      windowsCompleted: SNAPSHOT_FRAME.windowsCompleted,
      seqCursor: SNAPSHOT_FRAME.seqCursor,
    });
    await act(async () => {
      await vi.advanceTimersByTimeAsync(250);
    });

    expect(screen.getByText("Runner silent — session stale")).toBeInTheDocument();
    expect(screen.queryByText("Session live")).toBeNull();
  });
});
