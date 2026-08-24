//! WebSocket lifecycle for the live session page.
//!
//! Responsibilities are deliberately narrow: hold one socket, validate every
//! inbound frame against the wire schema, and drip validated frames into the
//! pure reducer on a fixed 250 ms cadence so a burst of pushes costs one
//! render, not one per frame. Reconnects back off exponentially and resume
//! by sequence cursor, scoped to the session they were earned in — a new
//! runner session restarts the ledger from its own zero, not ours.
//!
//! The reducer owns all display arithmetic; this hook never sums.

import { useEffect, useRef, useState } from "react";
import { ViewerFrameSchema, type ViewerFrame } from "./liveSchema";
import {
  applyLiveFrame,
  initialLiveSession,
  type LiveSessionState,
} from "./liveSession";

/** Render batching window: worst-case added latency for a position row. */
const FLUSH_INTERVAL_MS = 250;
const BASE_BACKOFF_MS = 500;
const MAX_BACKOFF_MS = 8_000;

export function useLiveSession(url: string): LiveSessionState {
  const [state, setState] = useState<LiveSessionState>(initialLiveSession);

  // Mutable collaborators live in refs so the effect body stays a stable
  // closure regardless of re-renders.
  const stateRef = useRef<LiveSessionState>(initialLiveSession);
  const pendingFrames = useRef<ViewerFrame[]>([]);
  const resumeCursor = useRef<{ runId: string | null; seq: number }>({
    runId: null,
    seq: 0,
  });
  const socketRef = useRef<WebSocket | null>(null);

  useEffect(() => {
    let disposed = false;
    let reconnectTimer: ReturnType<typeof setTimeout> | null = null;
    let attempt = 0;

    const commit = (next: LiveSessionState): void => {
      stateRef.current = next;
      setState(next);
      const header = next.session;
      if (header && header.runId === resumeCursor.current.runId) {
        resumeCursor.current.seq = Math.max(resumeCursor.current.seq, next.seqCursor);
      } else if (header) {
        resumeCursor.current = { runId: header.runId, seq: next.seqCursor };
      }
    };

    const flush = (): void => {
      if (pendingFrames.current.length === 0 || disposed) return;
      const frames = pendingFrames.current;
      pendingFrames.current = [];
      let next = stateRef.current;
      for (const frame of frames) {
        next = applyLiveFrame(next, frame, Date.now());
      }
      commit(next);
    };

    const flushTimer = setInterval(flush, FLUSH_INTERVAL_MS);

    const ingestMessage = (raw: unknown): void => {
      let parsed: unknown;
      try {
        parsed = JSON.parse(String(raw));
      } catch {
        console.error("live stream sent a non-JSON frame; ignoring");
        return;
      }
      const frame = ViewerFrameSchema.safeParse(parsed);
      if (!frame.success) {
        console.error("live stream frame failed schema validation; ignoring", frame.error.message);
        return;
      }
      pendingFrames.current.push(frame.data);
    };

    const connect = (): void => {
      if (disposed) return;
      let socket: WebSocket;
      try {
        socket = new WebSocket(url);
      } catch (error) {
        console.error("live stream URL is unusable", error);
        scheduleReconnect();
        return;
      }
      socketRef.current = socket;

      socket.addEventListener("open", () => {
        attempt = 0;
        commit({ ...stateRef.current, connection: "open" });
        // Ask for anything we missed — but only within the session that
        // produced our cursor; a fresh session restarts at its own zero.
        if (resumeCursor.current.runId !== null) {
          socket.send(
            JSON.stringify({ t: "resume", afterSeq: resumeCursor.current.seq }),
          );
        }
      });
      socket.addEventListener("message", (event) => ingestMessage(event.data));
      socket.addEventListener("close", () => {
        if (!disposed) scheduleReconnect();
      });
      socket.addEventListener("error", () => socket.close());
    };

    const scheduleReconnect = (): void => {
      if (disposed) return;
      commit({ ...stateRef.current, connection: "reconnecting" });
      attempt += 1;
      const capped = Math.min(BASE_BACKOFF_MS * 2 ** (attempt - 1), MAX_BACKOFF_MS);
      const jittered = capped * (0.75 + Math.random() * 0.5);
      reconnectTimer = setTimeout(connect, jittered);
    };

    connect();

    return () => {
      disposed = true;
      clearInterval(flushTimer);
      if (reconnectTimer !== null) clearTimeout(reconnectTimer);
      const socket = socketRef.current;
      if (socket && socket.readyState <= WebSocket.OPEN) {
        socket.close();
      }
      socketRef.current = null;
    };
  }, [url]);

  return state;
}
