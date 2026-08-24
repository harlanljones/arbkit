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
import { operatorCommandSchema, runnerFrameSchema } from "./wire";
import type { OperatorCommand } from "./wire";

/** Ingest bodies beyond this are refused before parsing. The runner's own
 * batches are ≤64 records; 512 KiB covers that many rich records with slack. */
const MAX_INGEST_BYTES = 512 * 1024;
/** Command bodies are three tiny shapes; anything larger is refused unread. */
const MAX_COMMAND_BYTES = 4 * 1024;
/** How far ahead the staleness alarm is scheduled after activity. */
const STALE_CHECK_MS = 10_000;
/** Retained operator commands. The runner pulls by id, so a brief offline
 * window still receives its orders; a long one loses the oldest — an honest
 * gap, never an unbounded buffer. */
const COMMAND_QUEUE_CAPACITY = 64;

export interface Env {
  ASSETS: Fetcher;
  POSITION_ROOM: DurableObjectNamespace;
  LIVE_INGEST_TOKEN?: string;
  LIVE_OPERATOR_TOKEN?: string;
}

interface QueuedCommand {
  id: number;
  receivedAtEpochMs: number;
  command: OperatorCommand;
}

export class PositionRoom {
  private readonly viewers = new Set<WebSocket>();
  private readonly session = new PositionSession();
  private readonly commandQueue: QueuedCommand[] = [];
  private nextCommandId = 1;

  constructor(
    private readonly ctx: DurableObjectState,
    _env: Env,
  ) {}

  async fetch(request: Request): Promise<Response> {
    const { pathname } = new URL(request.url);
    if (pathname.endsWith("/ingest")) return this.handleIngest(request);
    if (pathname.endsWith("/ws")) return this.handleViewer(request);
    if (pathname.endsWith("/command")) return this.handleOperatorCommand(request);
    if (pathname.endsWith("/commands")) return this.handleRunnerCommands(request);
    return new Response("not found", { status: 404 });
  }

  // -- operator commands ----------------------------------------------------

  /** Accepts one operator command for the queue. Authentication happened at
   * the worker edge; this is where the body earns its place — schema first,
   * then queue. The 202 means "queued", never "applied": only the runner's
   * risk gate can apply a command, and its `risk` frames are how everyone
   * learns that actually happened. */
  private async handleOperatorCommand(request: Request): Promise<Response> {
    if (request.method !== "POST") {
      return jsonError(405, "method not allowed");
    }
    const declared = Number(request.headers.get("content-length") ?? "0");
    if (declared > MAX_COMMAND_BYTES) {
      return jsonError(413, "command too large");
    }
    let parsed: unknown;
    let body = "";
    try {
      body = await request.text();
      parsed = JSON.parse(body);
    } catch {
      // An oversized-but-unparseable body still reads as too large.
      if (body.length > MAX_COMMAND_BYTES) {
        return jsonError(413, "command too large");
      }
      return jsonError(400, "command is not valid JSON");
    }
    const command = operatorCommandSchema.safeParse(parsed);
    if (!command.success) {
      return jsonError(400, "command failed schema validation");
    }
    const id = this.nextCommandId;
    this.nextCommandId += 1;
    this.commandQueue.push({
      id,
      receivedAtEpochMs: Date.now(),
      command: command.data,
    });
    if (this.commandQueue.length > COMMAND_QUEUE_CAPACITY) {
      this.commandQueue.splice(
        0,
        this.commandQueue.length - COMMAND_QUEUE_CAPACITY,
      );
    }
    return Response.json({ queued: id }, { status: 202 });
  }

  /** The runner's pull endpoint: everything queued after `afterId`, oldest
   * first, as NDJSON envelopes. Delivery is at-least-once — the runner must
   * apply each command idempotently and remember its own high-water id. */
  private handleRunnerCommands(request: Request): Response {
    if (request.method !== "GET") {
      return jsonError(405, "method not allowed");
    }
    const afterIdRaw = new URL(request.url).searchParams.get("afterId");
    const afterId = afterIdRaw === null ? 0 : Number(afterIdRaw);
    if (!Number.isInteger(afterId) || afterId < 0) {
      return jsonError(400, "afterId must be a non-negative integer");
    }
    const pending = this.commandQueue.filter((entry) => entry.id > afterId);
    if (pending.length === 0) {
      return new Response(null, { status: 204 });
    }
    const body = pending
      .map((entry) => JSON.stringify({ id: entry.id, command: entry.command }))
      .join("\n");
    return new Response(`${body}\n`, {
      status: 200,
      headers: { "content-type": "application/x-ndjson" },
    });
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
    let sessionStarted = false;
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
      if (frame.data.t === "session-start") sessionStarted = true;
      if (frame.data.t === "positions") relayItems = frame.data.items;
    }

    // A viewer may already be connected to the idle room when a runner
    // starts. Totals and positions do not carry the session header, so send a
    // full authoritative snapshot at the session boundary or the browser can
    // show a live cursor while still saying "awaiting session header".
    if (sessionStarted) {
      this.broadcast(snapshotFrame(this.session, -1));
    } else if (relayItems !== null) {
      // New rows first, then the totals they produced — a client applying
      // both in order always lands on the authoritative state.
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
