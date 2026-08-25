//! Component smoke test for the live view over a stubbed WebSocket.
//!
//! Drives the real production path — hook opens the socket, frames are
//! schema-validated and buffered, the 250 ms flush commits them to React,
//! and the KPI grid renders the authoritative integers — plus the resume
//! handshake on reconnect. No module mocks beyond the socket itself.

import { act, render, screen, within } from "@testing-library/react";
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

const RISK_STATE = {
  executionMode: "paper",
  killSwitch: true,
  maxStakePerLegCents: null,
  maxDailyLossCents: null,
  dailyLossUsedCents: null,
  maxOpenTrades: null,
  openTrades: null,
  minEdgeBps: null,
};

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
  risk: RISK_STATE,
  fills: [],
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
    unwindFailures: 0,
    ackMatched: 103,
    inFlightRemaining: 1,
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
    // The console's identity probe fires one fetch on mount; answer it
    // hermetically (401 = unauthenticated console).
    vi.stubGlobal(
      "fetch",
      vi.fn(() =>
        Promise.resolve(new Response(JSON.stringify({ error: "unauthorized" }), { status: 401 })),
      ),
    );
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
      risk: null,
      fills: [],
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
      risk: RISK_STATE,
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

  it("renders the micro-live session health panel from streamed counters", async () => {
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

    const panel = screen.getByRole("group", { name: "Micro-live session health" });
    expect(panel).toBeInTheDocument();
    // Honest zeros render as data, never hidden and never "—".
    // Attempted and Ack matched both read 103 in this fixture.
    expect(within(panel).getAllByText("103")).toHaveLength(2);
    expect(within(panel).getByText("0")).toBeInTheDocument(); // unwind failures
    expect(within(panel).getByText("1")).toBeInTheDocument(); // in-flight remaining
    // Phantom rate is derived from authoritative counts: 8/103 ≈ 7.77%,
    // shown against the paper baseline reference.
    expect(within(panel).getByText("7.77% of 103 attempted")).toBeInTheDocument();
    expect(within(panel).getByText("paper baseline 10.01%")).toBeInTheDocument();
    // The paper fixture enforces no caps, so the panel says so honestly.
    expect(within(panel).getAllByText("Not enforced")).toHaveLength(3);
  });

  it("fails inert with — when micro-live counters and caps are absent", async () => {
    render(<LivePoc url="ws://test/api/live/ws" />);
    await act(async () => {
      await vi.advanceTimersByTimeAsync(0);
    });
    const socket = lastSocket();
    socket.open();
    socket.message({
      ...SNAPSHOT_FRAME,
      risk: null,
      funnel: {
        attempted: 0,
        capitalShort: 0,
        clean: 0,
        proportional: 0,
        phantom: 0,
        brokenLeg: 0,
      },
    });
    await act(async () => {
      await vi.advanceTimersByTimeAsync(250);
    });

    const panel = screen.getByRole("group", { name: "Micro-live session health" });
    expect(panel).toBeInTheDocument();
    // A runner that has not reported counters or caps must not have them
    // invented: each renders "—".
    expect(within(panel).getAllByText("—")).toHaveLength(7);
    expect(within(panel).queryByText("Not enforced")).toBeNull();
  });

  it("reports a healthy stream with last-frame age while frames flow", async () => {
    render(<LivePoc url="ws://test/api/live/ws" />);
    await act(async () => {
      await vi.advanceTimersByTimeAsync(0);
    });
    const socket = lastSocket();
    socket.open();
    socket.message(SNAPSHOT_FRAME);
    await act(async () => {
      await vi.advanceTimersByTimeAsync(1_000);
    });

    const banner = screen.getByTestId("stream-health");
    expect(banner).toHaveTextContent("Stream live");
    // The age label is real: time has passed since the frame landed.
    expect(banner).toHaveTextContent(/last frame (\d+s|<1s) ago/);
  });

  it("declares a supposedly-live stream stale after the silence budget", async () => {
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
    expect(screen.getByTestId("stream-health")).toHaveTextContent("Stream live");

    // Silence past the 20s budget on a session claiming to be live: the
    // banner must call it stale and label the data's true age — without any
    // new frame arriving to refresh anything.
    await act(async () => {
      await vi.advanceTimersByTimeAsync(22_000);
    });

    const banner = screen.getByTestId("stream-health");
    expect(banner).toHaveTextContent("Stream silent");
    expect(banner).toHaveTextContent(/no frame for 2[12]s/);
    expect(banner).toHaveTextContent(/frozen as of its last frame/);

    // A resumed frame restores health and resets the age honestly.
    socket.message({ t: "hello", serverTimeEpochMs: 2 });
    await act(async () => {
      await vi.advanceTimersByTimeAsync(500);
    });
    expect(screen.getByTestId("stream-health")).toHaveTextContent("Stream live");
  });

  it("never calls an idle room stale — silence there is expected", async () => {
    render(<LivePoc url="ws://test/api/live/ws" />);
    await act(async () => {
      await vi.advanceTimersByTimeAsync(0);
    });
    const socket = lastSocket();
    socket.open();
    socket.message({
      t: "snapshot",
      status: "idle",
      session: null,
      risk: null,
      fills: [],
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

    // Far past the silence budget: still healthy. An empty room is not a
    // dead stream, and the page must not conflate the two.
    await act(async () => {
      await vi.advanceTimersByTimeAsync(30_000);
    });

    const banner = screen.getByTestId("stream-health");
    expect(banner).toHaveTextContent("Stream live");
    expect(banner).toHaveTextContent(/waiting for a runner/);
    expect(screen.getByText("Waiting for a runner")).toBeInTheDocument(); // session pill
  });

  it("shows the disconnected state while the socket is down", async () => {
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
    expect(screen.getByTestId("stream-health")).toHaveTextContent("Stream live");

    socket.close();
    await act(async () => {
      await vi.advanceTimersByTimeAsync(100);
    });

    const banner = screen.getByTestId("stream-health");
    expect(banner).toHaveTextContent("Stream disconnected");
    expect(banner).toHaveTextContent(/Reconnecting/);
  });
});
