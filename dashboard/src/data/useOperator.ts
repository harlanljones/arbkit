//! Operator command transport for the live console.
//!
//! One narrow job: POST a validated-shape command to the worker's
//! authenticated operator endpoint and report what the worker said. A 202 is
//! an ack of *queuing*, never of application — only the runner's risk gate
//! applies commands, and its `risk` frames are the authoritative echo. This
//! hook holds no state about whether a command "took": the session stream
//! owns that truth.

import { useCallback, useRef, useState } from "react";

export type OperatorCommand =
  | { t: "session-start"; mode: "paper" | "live" }
  | { t: "session-end" }
  | { t: "kill-switch"; engage: boolean };

export type OperatorSendResult =
  | { ok: true; queuedId: number }
  | { ok: false; error: string };

export interface OperatorController {
  send: (command: OperatorCommand) => Promise<OperatorSendResult>;
  /** A POST is on the wire. Commands are one-at-a-time so the console can
   * never interleave two orders of operations. */
  pending: boolean;
  lastError: string | null;
  lastQueuedAtMs: number | null;
  lastQueuedId: number | null;
}

function defaultOperatorUrl(): string {
  if (typeof window === "undefined") return "/api/live/command";
  return `${window.location.origin}/api/live/command`;
}

export function useOperator(url?: string): OperatorController {
  const endpoint = url ?? defaultOperatorUrl();
  const [pending, setPending] = useState(false);
  const [lastError, setLastError] = useState<string | null>(null);
  const [lastQueuedAtMs, setLastQueuedAtMs] = useState<number | null>(null);
  const [lastQueuedId, setLastQueuedId] = useState<number | null>(null);
  const inFlight = useRef(false);

  const send = useCallback(
    async (command: OperatorCommand): Promise<OperatorSendResult> => {
      if (inFlight.current) {
        return { ok: false, error: "another command is still in flight" };
      }
      inFlight.current = true;
      setPending(true);
      setLastError(null);
      try {
        const response = await fetch(endpoint, {
          method: "POST",
          headers: { "content-type": "application/json" },
          body: JSON.stringify(command),
        });
        if (response.status === 202) {
          const body = (await response.json().catch(() => null)) as
            | { queued?: unknown }
            | null;
          const queuedId = typeof body?.queued === "number" ? body.queued : null;
          setLastQueuedAtMs(Date.now());
          setLastQueuedId(queuedId);
          return { ok: true, queuedId: queuedId ?? -1 };
        }
        const detail = await response.json().catch(() => null);
        const message =
          typeof detail?.error === "string"
            ? detail.error
            : `command refused (HTTP ${response.status})`;
        setLastError(message);
        return { ok: false, error: message };
      } catch (error) {
        const message = error instanceof Error ? error.message : "command transport failed";
        setLastError(message);
        return { ok: false, error: message };
      } finally {
        inFlight.current = false;
        setPending(false);
      }
    },
    [endpoint],
  );

  return { send, pending, lastError, lastQueuedAtMs, lastQueuedId };
}
