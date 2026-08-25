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
  | { t: "kill-switch"; engage: true }
  | { t: "kill-switch"; engage: false; confirm: true };

export type OperatorSendResult =
  | { ok: true; queuedId: number }
  | { ok: false; error: string };

/** One audited command this console sent, newest-first in the log. A refusal
 * carries the worker's verbatim reason — refusals are evidence, not noise.
 * Whether a queued command took effect is deliberately absent: only the
 * runner's own `risk`/session frames prove that, and the audit trail derives
 * it from the live stream at render time rather than storing a guess. */
export interface CommandAuditEntry {
  /** Worker-assigned queue id; null when refused before queueing. */
  id: number | null;
  command: OperatorCommand;
  sentAtMs: number;
  status: "refused" | "queued";
  error?: string;
  /** The worker-attested operator name for the session that sent this, as
   * reported by `/api/live/auth/session`. Absent means unattributed: no
   * session existed (break-glass bearer path) or the entry predates
   * identity on the wire. Rendered as "—", never invented. */
  issuer?: string;
}

/** Per-send attribution context. The console fills it from its cached,
 * server-attested session identity; the worker re-verifies independently. */
export interface SendContext {
  issuer?: string;
}

export interface OperatorController {
  send: (command: OperatorCommand, context?: SendContext) => Promise<OperatorSendResult>;
  /** A POST is on the wire. Commands are one-at-a-time so the console can
   * never interleave two orders of operations. */
  pending: boolean;
  lastError: string | null;
  lastQueuedAtMs: number | null;
  lastQueuedId: number | null;
  /** Newest-first record of every command this console sent that reached
   * the worker, refused or queued. */
  auditLog: CommandAuditEntry[];
}

/** Recent commands retained for the audit trail. */
const MAX_AUDIT_ENTRIES = 24;

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
  const [auditLog, setAuditLog] = useState<CommandAuditEntry[]>([]);
  const inFlight = useRef(false);

  const audit = useCallback((entry: CommandAuditEntry): void => {
    setAuditLog((previous) =>
      [entry, ...previous].slice(0, MAX_AUDIT_ENTRIES),
    );
  }, []);

  const send = useCallback(
    async (command: OperatorCommand, context?: SendContext): Promise<OperatorSendResult> => {
      if (inFlight.current) {
        // A local guard, not a worker verdict: nothing reached anyone, so
        // there is nothing to audit.
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
          const sentAtMs = Date.now();
          setLastQueuedAtMs(sentAtMs);
          setLastQueuedId(queuedId);
          audit({
            id: queuedId,
            command,
            sentAtMs,
            status: "queued",
            issuer: context?.issuer,
          });
          return { ok: true, queuedId: queuedId ?? -1 };
        }
        const detail = await response.json().catch(() => null);
        const message =
          typeof detail?.error === "string"
            ? detail.error
            : `command refused (HTTP ${response.status})`;
        setLastError(message);
        audit({
          id: null,
          command,
          sentAtMs: Date.now(),
          status: "refused",
          error: message,
          issuer: context?.issuer,
        });
        return { ok: false, error: message };
      } catch (error) {
        const message = error instanceof Error ? error.message : "command transport failed";
        setLastError(message);
        audit({
          id: null,
          command,
          sentAtMs: Date.now(),
          status: "refused",
          error: message,
          issuer: context?.issuer,
        });
        return { ok: false, error: message };
      } finally {
        inFlight.current = false;
        setPending(false);
      }
    },
    [endpoint, audit],
  );

  return { send, pending, lastError, lastQueuedAtMs, lastQueuedId, auditLog };
}
