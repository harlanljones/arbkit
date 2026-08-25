//! Component tests for the operator console.
//!
//! These pin the safety behaviors the ticket names: the kill switch gates
//! every order-entry control, an engaged posture renders even when the
//! stream is down, a disconnected console is inert no matter what its cached
//! state says, and open settlements show locked capital without fabricating
//! realized cents. The operator transport is a fake recorder — the console's
//! contract with it is one call per click, nothing more.

import { fireEvent, render, screen, within } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

import type { LiveSessionState } from "../data/liveSession";
import { initialLiveSession } from "../data/liveSession";
import type { RiskState } from "../data/liveSchema";
import type {
  CommandAuditEntry,
  OperatorCommand,
  OperatorController,
} from "../data/useOperator";
import { OperatorConsole } from "./OperatorConsole";

const DISARMED_PAPER: RiskState = {
  executionMode: "paper",
  killSwitch: false,
  maxStakePerLegCents: 5_000,
  maxDailyLossCents: 50_000,
  dailyLossUsedCents: 12_345,
  maxOpenTrades: 1,
  openTrades: 0,
  minEdgeBps: 50,
};

const ENGAGED_LIVE: RiskState = {
  executionMode: "live",
  killSwitch: true,
  maxStakePerLegCents: 5_000,
  maxDailyLossCents: 50_000,
  dailyLossUsedCents: 0,
  maxOpenTrades: 2,
  openTrades: 1,
  minEdgeBps: 75,
};

function liveState(overrides: Partial<LiveSessionState> = {}): LiveSessionState {
  return { ...initialLiveSession, ...overrides };
}

function fakeOperator(
  overrides: Partial<OperatorController> = {},
): OperatorController & { commands: OperatorCommand[] } {
  const commands: OperatorCommand[] = [];
  return {
    send: async (command, _context) => {
      commands.push(command);
      return { ok: true, queuedId: commands.length };
    },
    pending: false,
    lastError: null,
    lastQueuedAtMs: null,
    lastQueuedId: null,
    auditLog: [],
    commands,
    ...overrides,
  };
}

describe("OperatorConsole", () => {
  it("treats an unknown posture as engaged and offers no controls", () => {
    render(<OperatorConsole live={liveState()} operator={fakeOperator()} />);

    expect(screen.getByTestId("kill-switch-state")).toHaveTextContent("Kill switch engaged");
    expect(screen.getByTestId("execution-mode")).toHaveTextContent("Mode: unknown");
    expect(screen.getByTestId("order-entry")).toHaveAttribute("data-open", "false");
    // Nothing is reachable before the stream itself is up.
    expect(screen.getByRole("button", { name: /Start session/ })).toBeDisabled();
    expect(screen.getByRole("button", { name: "Arm" })).toBeDisabled();
  });

  it("renders the engaged posture even while disconnected", () => {
    render(
      <OperatorConsole
        live={liveState({ connection: "reconnecting", risk: ENGAGED_LIVE })}
        operator={fakeOperator()}
      />,
    );

    expect(screen.getByTestId("kill-switch-state")).toHaveTextContent("Kill switch engaged");
    expect(screen.getByTestId("execution-mode")).toHaveTextContent("Mode: live");
    expect(screen.getByText(/Console disconnected/)).toHaveAttribute("role", "alert");
    expect(screen.getByTestId("order-entry")).toHaveAttribute("data-open", "false");
  });

  it("fails inert when disconnected even if the cached switch reads disarmed", () => {
    render(
      <OperatorConsole
        live={liveState({
          connection: "reconnecting",
          sessionStatus: "live",
          risk: DISARMED_PAPER,
        })}
        operator={fakeOperator()}
      />,
    );

    // Cached disarm is display-only; no cached state may look like authority.
    expect(screen.getByTestId("kill-switch-state")).toHaveTextContent("Kill switch disarmed");
    expect(screen.getByText(/controls are inert/)).toBeInTheDocument();
    expect(screen.getByTestId("order-entry")).toHaveAttribute("data-open", "false");
    for (const name of [/Start session/, "End session", "Arm", "Disarm"]) {
      expect(screen.getByRole("button", { name })).toBeDisabled();
    }
  });

  it("opens order entry only when connected, disarmed, and the session is live", () => {
    const operator = fakeOperator();
    render(
      <OperatorConsole
        live={liveState({
          connection: "open",
          sessionStatus: "live",
          risk: DISARMED_PAPER,
        })}
        operator={operator}
      />,
    );

    expect(screen.getByTestId("order-entry")).toHaveAttribute("data-open", "true");

    // Paper starts immediately; live demands its own confirmation first.
    fireEvent.click(screen.getByLabelText("Live — real capital"));
    expect(screen.getByRole("button", { name: /Start session \(live\)/ })).toBeDisabled();
    fireEvent.click(screen.getByLabelText("Confirm live mode before start"));
    const start = screen.getByRole("button", { name: /Start session \(live\)/ });
    expect(start).toBeEnabled();

    fireEvent.click(start);
    expect(operator.commands).toEqual([{ t: "session-start", mode: "live" }]);
  });

  it("disables disarm until explicitly confirmed, and arm once already armed", () => {
    const operator = fakeOperator();
    render(
      <OperatorConsole
        live={liveState({ connection: "open", sessionStatus: "live", risk: ENGAGED_LIVE })}
        operator={operator}
      />,
    );

    const disarm = screen.getByRole("button", { name: "Disarm" });
    expect(disarm).toBeDisabled();
    fireEvent.click(screen.getByLabelText(/Confirm disarming/));
    expect(disarm).toBeEnabled();
    fireEvent.click(disarm);
    expect(operator.commands).toEqual([{ t: "kill-switch", engage: false, confirm: true }]);

    // The runner already reports engaged, so arming again is a no-op request
    // the console refuses to spend a command on.
    expect(screen.getByRole("button", { name: "Arm" })).toBeDisabled();
  });

  it("shows the risk envelope straight from the runner, including unenforced caps", () => {
    render(
      <OperatorConsole
        live={liveState({ connection: "open", sessionStatus: "live", risk: DISARMED_PAPER })}
        operator={fakeOperator()}
      />,
    );

    const summary = screen.getByTestId("posture-summary");
    expect(summary).toHaveTextContent("$50.00"); // per-leg cap
    expect(summary).toHaveTextContent("$376.55 remaining"); // 50_000 - 12_345 cents
    expect(summary).toHaveTextContent("0 of 1"); // open trades
    expect(summary).toHaveTextContent("50 bps"); // edge floor
  });

  it("renders open settlements as locked capital and never as realized cents", () => {
    render(
      <OperatorConsole
        live={liveState({
          connection: "open",
          sessionStatus: "live",
          risk: DISARMED_PAPER,
          items: [
            {
              seq: 7,
              detectionTimestampNs: 1,
              latencyNs: 1,
              marketLabel: "BOS @ LAL · moneyline",
              edgeBps: 400,
              overroundPpm: 960_000,
              requestedStakeCents: 100_000,
              expectedProfitCents: 4_000,
              worstCaseProfitCents: 4_000,
              realizedProfitCents: null,
              slippageCents: 0,
              feesPaidCents: 0,
              fillRatioBps: 10_000,
              classification: "clean",
              chased: false,
              legs: [],
              executionMode: "live",
              venueOrderIds: ["kalshi-9", "poly-ox51"],
              filledStakeCents: 99_500,
              settlementStatus: "open",
            },
            {
              seq: 8,
              detectionTimestampNs: 2,
              latencyNs: 1,
              marketLabel: "BOS @ LAL · moneyline",
              edgeBps: 380,
              overroundPpm: 961_000,
              requestedStakeCents: 100_000,
              expectedProfitCents: 3_800,
              worstCaseProfitCents: 3_800,
              realizedProfitCents: 3_100,
              slippageCents: 0,
              feesPaidCents: 0,
              fillRatioBps: 10_000,
              classification: "clean",
              chased: false,
              legs: [],
              executionMode: "live",
              settlementStatus: "settled",
            },
          ],
        })}
        operator={fakeOperator()}
      />,
    );

    const table = screen.getByTestId("open-positions");
    // Only the open trade sits in the locked-capital view (header + one row)...
    expect(within(table).getAllByRole("row")).toHaveLength(2);
    // ...with the filled stake locked, venue order ids, and no fabricated outcome.
    expect(table).toHaveTextContent("$995.00");
    expect(table).toHaveTextContent("kalshi-9, poly-ox51");
    expect(table).toHaveTextContent("Open");
    // The settled trade stays out entirely: its $31.00 of realized profit
    // belongs to the ledger below, not to an unsettled-capital view.
    expect(table).not.toHaveTextContent("$31.00");
  });

  it("lists reconciled fills keyed by client and venue order id", () => {
    render(
      <OperatorConsole
        live={liveState({
          connection: "open",
          risk: DISARMED_PAPER,
          fills: [
            {
              clientOrderId: "cid-abc",
              venueOrderId: "vid-77",
              tradeSeq: 4,
              filledStakeCents: 50_000,
              realizedProfitCents: null,
              settlementStatus: "open",
              reconciledAtEpochMs: 1_000,
            },
          ],
        })}
        operator={fakeOperator()}
      />,
    );

    const feed = screen.getByTestId("fill-feed");
    expect(feed).toHaveTextContent("cid-abc");
    expect(feed).toHaveTextContent("vid-77");
    expect(feed).toHaveTextContent("$500.00 filled");
    // No realized cents are claimed before settlement reports them.
    expect(feed).not.toHaveTextContent("realized");
  });

  it("surfaces a refused command instead of silently retrying", () => {
    render(
      <OperatorConsole
        live={liveState({ connection: "open", risk: DISARMED_PAPER })}
        operator={fakeOperator({ lastError: "command failed schema validation" })}
      />,
    );

    expect(screen.getByRole("status")).toHaveTextContent(
      "Last command refused: command failed schema validation",
    );
  });

  it("shows an empty command trail before anything was sent", () => {
    render(
      <OperatorConsole
        live={liveState({ connection: "open", risk: DISARMED_PAPER })}
        operator={fakeOperator()}
      />,
    );

    const trail = screen.getByTestId("command-audit");
    expect(trail).toHaveTextContent("No commands sent from this console yet.");
  });

  it("renders queued, in-effect, and refused commands with ids and reasons", () => {
    const sentAt = Date.parse("2026-08-24T12:00:00Z");
    const entries: CommandAuditEntry[] = [
      // Posture matches the commanded engage=true → the runner's own risk
      // frame proves it is in effect.
      {
        id: 4,
        command: { t: "kill-switch", engage: true },
        sentAtMs: sentAt,
        status: "queued",
      },
      // Session-end against a still-live session stays queued.
      {
        id: 3,
        command: { t: "session-end" },
        sentAtMs: sentAt - 1_000,
        status: "queued",
      },
      // Worker-edge refusal carries its verbatim reason.
      {
        id: null,
        command: { t: "session-start", mode: "live" },
        sentAtMs: sentAt - 2_000,
        status: "refused",
        error: "command failed schema validation",
      },
    ];
    render(
      <OperatorConsole
        live={liveState({ connection: "open", sessionStatus: "live", risk: ENGAGED_LIVE })}
        operator={fakeOperator({ auditLog: entries })}
      />,
    );

    const trail = screen.getByTestId("command-audit");
    const rows = within(trail).getAllByRole("listitem");
    expect(rows).toHaveLength(3);

    // Arm command: posture already matches, so the runner's own frame
    // proves it is in effect.
    expect(rows[0]).toHaveTextContent("Arm kill switch");
    expect(rows[0]).toHaveTextContent("#4");
    expect(within(rows[0]).getByText("In effect")).toBeInTheDocument();

    // End-session while the session is still live: queued, honestly.
    expect(rows[1]).toHaveTextContent("End session");
    expect(rows[1]).toHaveTextContent("#3");
    expect(within(rows[1]).getByText("Queued")).toBeInTheDocument();

    // Refusal shows the worker's verbatim reason — evidence, not noise.
    expect(rows[2]).toHaveTextContent("Start session (live)");
    expect(within(rows[2]).getByText("Refused")).toBeInTheDocument();
    expect(
      within(rows[2]).getByText("command failed schema validation"),
    ).toBeInTheDocument();
  });

  it("reads a disarm as in effect once the runner's own posture matches", () => {
    const entry: CommandAuditEntry = {
      id: 9,
      command: { t: "kill-switch", engage: false, confirm: true },
      sentAtMs: Date.parse("2026-08-24T12:00:00Z"),
      status: "queued",
    };
    render(
      <OperatorConsole
        live={liveState({ connection: "open", sessionStatus: "live", risk: DISARMED_PAPER })}
        operator={fakeOperator({ auditLog: [entry] })}
      />,
    );

    const trail = screen.getByTestId("command-audit");
    expect(within(trail).getByText("Disarm kill switch")).toBeInTheDocument();
    expect(within(trail).getByText("#9")).toBeInTheDocument();
    expect(within(trail).getByText("In effect")).toBeInTheDocument();
    // A disarmed posture means order entry is open; no refusal to show.
    expect(within(trail).queryByText("Refused")).toBeNull();
  });

  it("marks an ended session's end-command in effect from the session frames", () => {
    const entry: CommandAuditEntry = {
      id: 11,
      command: { t: "session-end" },
      sentAtMs: Date.parse("2026-08-24T12:00:00Z"),
      status: "queued",
    };
    render(
      <OperatorConsole
        live={liveState({ connection: "open", sessionStatus: "ended", risk: ENGAGED_LIVE })}
        operator={fakeOperator({ auditLog: [entry] })}
      />,
    );

    const trail = screen.getByTestId("command-audit");
    expect(within(trail).getByText("End session")).toBeInTheDocument();
    expect(within(trail).getByText("In effect")).toBeInTheDocument();
  });

  it("shows the worker-attested issuer per row and — for unattributed sends", () => {
    const sentAt = Date.parse("2026-08-24T12:00:00Z");
    const entries: CommandAuditEntry[] = [
      // Attributed queued command: the session name rides the row.
      {
        id: 5,
        command: { t: "kill-switch", engage: false, confirm: true },
        sentAtMs: sentAt,
        status: "queued",
        issuer: "harlan",
      },
      // Refusals keep their verbatim reason AND their attribution.
      {
        id: null,
        command: { t: "session-start", mode: "live" },
        sentAtMs: sentAt - 1_000,
        status: "refused",
        error: "command failed schema validation",
        issuer: "harlan",
      },
      // Unattributed (break-glass or pre-identity): an honest dash.
      {
        id: 2,
        command: { t: "session-end" },
        sentAtMs: sentAt - 2_000,
        status: "queued",
      },
    ];
    render(
      <OperatorConsole
        live={liveState({ connection: "open", sessionStatus: "ended", risk: ENGAGED_LIVE })}
        operator={fakeOperator({ auditLog: entries })}
      />,
    );

    const trail = screen.getByTestId("command-audit");
    const rows = within(trail).getAllByRole("listitem");
    expect(rows).toHaveLength(3);
    expect(within(rows[0]).getByText("harlan")).toBeInTheDocument();
    expect(within(rows[1]).getByText("harlan")).toBeInTheDocument();
    expect(within(rows[1]).getByText("command failed schema validation")).toBeInTheDocument();
    expect(within(rows[2]).getByText("—")).toBeInTheDocument();
    // Lifecycle semantics untouched by the identity column: the disarm sits
    // queued against the engaged posture; the end-command reads in effect.
    expect(within(trail).getAllByText("In effect")).toHaveLength(1);
    expect(within(rows[0]).getByText("Queued")).toBeInTheDocument();
  });
});
