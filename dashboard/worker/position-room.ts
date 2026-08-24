//! `PositionRoom` — the single live-session Durable Object.
//!
//! One global instance (id name `"live"`): the runner POSTs validated-then-
//! applied NDJSON batches, connected browsers receive authoritative frames
//! over plain WebSockets. State is deliberately in-memory — a session's life
//! is exactly its runner's life, and heartbeat staleness is the goodbye.
//! Hibernation would preserve sockets, not sessions, so it buys nothing here.
//!
//! The browser never sums: this room owns the arithmetic via
//! [`PositionSession`], and every push either carries new ledger rows
//! (`positions`) or the full authoritative aggregates (`totals`).

import {
  PositionSession,
  helloFrame,
  snapshotFrame,
  totalsFrame,
} from "./state";
import { runnerFrameSchema } from "./wire";

/** Ingest bodies beyond this are refused before parsing. The runner's own
 * batches are ≤64 records; 512 KiB covers that many rich records with slack. */
const MAX_INGEST_BYTES = 512 * 1024;
/** How far ahead the staleness alarm is scheduled after activity. */
const STALE_CHECK_MS = 10_000;

export interface Env {
  ASSETS: Fetcher;
  POSITION_ROOM: DurableObjectNamespace;
  LIVE_INGEST_TOKEN?: string;
}

export class PositionRoom {
  private readonly viewers = new Set<WebSocket>();
  private readonly session = new PositionSession();

  constructor(
    private readonly ctx: DurableObjectState,
    _env: Env,
  ) {}

  async fetch(request: Request): Promise<Response> {
    const { pathname } = new URL(request.url);
    if (pathname.endsWith("/ingest")) return this.handleIngest(request);
    if (pathname.endsWith("/ws")) return this.handleViewer(request);
    return new Response("not found", { status: 404 });
  }

  // -- ingest ---------------------------------------------------------------

  private async handleIngest(request: Request): Promise<Response> {
    if (request.method !== "POST") {
      return new Response("method not allowed", { status: 405 });
    }
    const declared = Number(request.headers.get("content-length") ?? "0");
    if (declared > MAX_INGEST_BYTES) {
      return new Response("batch too large", { status: 413 });
    }

    const text = await request.text();
    if (text.length > MAX_INGEST_BYTES) {
      return new Response("batch too large", { status: 413 });
    }

    const now = Date.now();
    let relayItems: unknown[] | null = null;
    const lines = text.split("\n").filter((line) => line.trim().length > 0);

    for (let index = 0; index < lines.length; index += 1) {
      let parsed: unknown;
      try {
        parsed = JSON.parse(lines[index]);
      } catch {
        return jsonError(400, `line ${index + 1} is not valid JSON`);
      }
      const frame = runnerFrameSchema.safeParse(parsed);
      if (!frame.success) {
        return jsonError(400, `line ${index + 1} failed schema validation`);
      }
      if (
        frame.data.t !== "session-start" &&
        this.session.getStatus() === "ended"
      ) {
        return jsonError(
          409,
          `line ${index + 1}: session already ended; send session-start to begin anew`,
        );
      }
      this.session.apply(frame.data, now);
      if (frame.data.t === "positions") relayItems = frame.data.items;
    }

    // New rows first, then the totals they produced — a client applying both
    // in order always lands on the authoritative state.
    if (relayItems !== null) {
      this.broadcast({ t: "positions", items: relayItems });
    }
    this.broadcast(totalsFrame(this.session));
    await this.ensureStaleAlarm(now);

    return new Response(null, { status: 204 });
  }

  // -- viewers --------------------------------------------------------------

  private handleViewer(request: Request): Response {
    if (request.headers.get("upgrade")?.toLowerCase() !== "websocket") {
      return new Response("expected websocket upgrade", { status: 426 });
    }

    const pair = new WebSocketPair();
    const server = pair[1];
    server.accept();
    this.viewers.add(server);

    server.addEventListener("message", (event) => {
      // The only message a viewer may send: resume from a seq cursor.
      try {
        const parsed = JSON.parse(String(event.data)) as { t?: string; afterSeq?: number };
        if (parsed.t === "resume" && typeof parsed.afterSeq === "number") {
          this.sendTo(server, snapshotFrame(this.session, Math.floor(parsed.afterSeq)));
        }
      } catch {
        // Unparseable chatter is ignored, not fatal.
      }
    });

    const remove = (): void => {
      this.viewers.delete(server);
    };
    server.addEventListener("close", remove);
    server.addEventListener("error", remove);

    this.sendTo(server, helloFrame(Date.now()));
    this.sendTo(server, snapshotFrame(this.session, -1));

    return new Response(null, { status: 101, webSocket: pair[0] });
  }

  // -- fan-out & liveness ---------------------------------------------------

  private broadcast(message: unknown): void {
    const data = JSON.stringify(message);
    for (const viewer of [...this.viewers]) {
      try {
        viewer.send(data);
      } catch {
        this.viewers.delete(viewer);
      }
    }
  }

  private sendTo(viewer: WebSocket, message: unknown): void {
    try {
      viewer.send(JSON.stringify(message));
    } catch {
      this.viewers.delete(viewer);
    }
  }

  private async ensureStaleAlarm(now: number): Promise<void> {
    const existing = await this.ctx.storage.getAlarm();
    if (existing !== null && existing <= now + STALE_CHECK_MS) return;
    await this.ctx.storage.setAlarm(now + STALE_CHECK_MS);
  }

  /** Cloudflare invokes this when the scheduled staleness check fires. */
  async alarm(): Promise<void> {
    if (this.session.markStaleIfExpired(Date.now())) {
      this.broadcast(totalsFrame(this.session));
    }
    // A still-live session reschedules; an idle one lets alarms die out.
    if (this.session.getStatus() === "live") {
      await this.ensureStaleAlarm(Date.now());
    }
  }
}

function jsonError(status: number, message: string): Response {
  return Response.json({ error: message }, { status });
}
