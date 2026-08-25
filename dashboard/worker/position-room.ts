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
import {
  CLEAR_SESSION_COOKIE,
  CONSOLE_HEADER,
  OperatorAuth,
  parseRoster,
  readSessionToken,
  sessionCookie,
  SESSION_TTL_MS,
} from "./session";
import { operatorCommandSchema, runnerFrameSchema } from "./wire";
import type { OperatorCommand } from "./wire";

/** Ingest bodies beyond this are refused before parsing. The runner's own
 * batches are ≤64 records; 512 KiB covers that many rich records with slack. */
const MAX_INGEST_BYTES = 512 * 1024;
/** Command bodies are three tiny shapes; anything larger is refused unread. */
const MAX_COMMAND_BYTES = 4 * 1024;
/** Auth bodies are one key id / one challenge answer; anything larger is
 * refused unread. */
const MAX_AUTH_BYTES = 2 * 1024;
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
  /** JSON roster of operators: `[{keyId, name, publicKeyPem}]` (SPKI public
   * keys only). Absent or malformed refuses all authentication — fail closed. */
  LIVE_OPERATOR_ROSTER?: string;
}

interface QueuedCommand {
  id: number;
  receivedAtEpochMs: number;
  /** The worker's own attestation of who issued this: a verified session
   * name, or the fixed legacy label while the shared bearer token still
   * works (HJ-313 decides its fate). Never accepted from client input. */
  operator: string;
  command: OperatorCommand;
}

/** What the legacy shared token can attest: that an anonymous holder of the
 * secret presented it. A person's name comes only from a verified session. */
const LEGACY_BEARER_OPERATOR = "anonymous-operator-token";

export class PositionRoom {
  private readonly viewers = new Set<WebSocket>();
  private readonly session = new PositionSession();
  private readonly commandQueue: QueuedCommand[] = [];
  private readonly auth: OperatorAuth;
  private readonly env: Env;
  private nextCommandId = 1;

  constructor(
    private readonly ctx: DurableObjectState,
    env: Env,
  ) {
    this.env = env;
    this.auth = new OperatorAuth(parseRoster(env.LIVE_OPERATOR_ROSTER));
  }

  async fetch(request: Request): Promise<Response> {
    const { pathname } = new URL(request.url);
    if (pathname.endsWith("/ingest")) return this.handleIngest(request);
    if (pathname.endsWith("/ws")) return this.handleViewer(request);
    if (pathname.endsWith("/command")) return this.handleOperatorCommand(request);
    if (pathname.endsWith("/commands")) return this.handleRunnerCommands(request);
    if (pathname.endsWith("/auth/challenge")) return this.handleAuthChallenge(request);
    if (pathname.endsWith("/auth/login")) return this.handleAuthLogin(request);
    if (pathname.endsWith("/auth/logout")) return this.handleAuthLogout(request);
    if (pathname.endsWith("/auth/session")) return this.handleAuthStatus(request);
    return new Response("not found", { status: 404 });
  }

  // -- operator commands ----------------------------------------------------

  /** Command authority lives HERE, next to the session store it checks —
   * not at the worker edge, which cannot see sessions. Priority: a verified
   * operator session attests its name; during migration the shared bearer
   * token still works and attests only itself; with no mechanism configured
   * the surface refuses service exactly as before (fail closed). */
  private commandIssuer(
    request: Request,
  ): { ok: true; operator: string } | { ok: false; status: number; error: string } {
    const session = this.auth.validate(readSessionToken(request), Date.now());
    if (session !== null) return { ok: true, operator: session.operator };

    const configured = this.env.LIVE_OPERATOR_TOKEN;
    const presented = request.headers.get("authorization") ?? "";
    if (
      configured !== undefined &&
      PositionRoom.timingSafeEqual(presented, `Bearer ${configured}`)
    ) {
      return { ok: true, operator: LEGACY_BEARER_OPERATOR };
    }
    if (configured === undefined && !this.auth.available) {
      return {
        ok: false,
        status: 503,
        error:
          "no operator authority configured (set LIVE_OPERATOR_TOKEN or provision LIVE_OPERATOR_ROSTER)",
      };
    }
    return { ok: false, status: 401, error: "unauthorized" };
  }

  /** Length-then-byte comparison; same rationale as the worker edge. */
  private static timingSafeEqual(presented: string, expected: string): boolean {
    if (presented.length !== expected.length) return false;
    let mismatch = 0;
    for (let index = 0; index < expected.length; index += 1) {
      mismatch |= presented.charCodeAt(index) ^ expected.charCodeAt(index);
    }
    return mismatch === 0;
  }

  /** Accepts one operator command for the queue. Authentication happens here
   * first — a request that carries no authority never reaches parsing. The
   * 202 means "queued", never "applied": only the runner's risk gate can
   * apply a command, and its `risk` frames are how everyone learns that
   * actually happened. */
  private async handleOperatorCommand(request: Request): Promise<Response> {
    if (request.method !== "POST") {
      return jsonError(405, "method not allowed");
    }
    const issuer = this.commandIssuer(request);
    if (!issuer.ok) return jsonError(issuer.status, issuer.error);
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
      operator: issuer.operator,
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
   * first, as NDJSON envelopes carrying the worker-attested issuer and the
   * queue timestamp alongside each command. Delivery is at-least-once — the
   * runner must apply each command idempotently and remember its own
   * high-water id. Old runners ignore the extra fields (serde default). */
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
      .map((entry) =>
        JSON.stringify({
          id: entry.id,
          receivedAtEpochMs: entry.receivedAtEpochMs,
          operator: entry.operator,
          command: entry.command,
        }),
      )
      .join("\n");
    return new Response(`${body}\n`, {
      status: 200,
      headers: { "content-type": "application/x-ndjson" },
    });
  }

  // -- operator authentication ----------------------------------------------

  /** Every auth mutation must carry the console header: cross-site forms
   * cannot send it without a CORS preflight, which closes the CSRF path the
   * SameSite=Strict cookie does not already cover. */
  private static readonly JSON_BODY_LIMIT = MAX_AUTH_BYTES;

  private static consoleGuarded(request: Request): boolean {
    return (
      request.headers.get(CONSOLE_HEADER) !== null &&
      (request.headers.get("content-type") ?? "").includes("application/json")
    );
  }

  private static async readJsonBody<T>(request: Request): Promise<T | null> {
    const declared = Number(request.headers.get("content-length") ?? "0");
    if (declared > PositionRoom.JSON_BODY_LIMIT) return null;
    try {
      const body = await request.text();
      if (body.length > PositionRoom.JSON_BODY_LIMIT) return null;
      return JSON.parse(body) as T;
    } catch {
      return null;
    }
  }

  /** Step one of login: a single-use nonce bound to a roster key. Unknown
   * key ids get the same uniform 401 as a bad signature — responses never
   * enumerate the roster. */
  private async handleAuthChallenge(request: Request): Promise<Response> {
    if (request.method !== "POST") return jsonError(405, "method not allowed");
    if (!PositionRoom.consoleGuarded(request)) return jsonError(403, "missing console header");
    const body = await PositionRoom.readJsonBody<{ keyId?: unknown }>(request);
    if (body === null || typeof body.keyId !== "string" || body.keyId === "") {
      return jsonError(400, "keyId required");
    }
    if (!this.auth.available) {
      return Response.json(
        { error: "operator authentication not configured" },
        { status: 503 },
      );
    }
    const challenge = this.auth.issueChallenge(body.keyId, Date.now());
    if (challenge === null) return jsonError(401, "authentication failed");
    return Response.json(challenge);
  }

  /** Step two of login: verify the challenge signature against the roster's
   * registered public key and issue the session cookie. */
  private async handleAuthLogin(request: Request): Promise<Response> {
    if (request.method !== "POST") return jsonError(405, "method not allowed");
    if (!PositionRoom.consoleGuarded(request)) return jsonError(403, "missing console header");
    const body = await PositionRoom.readJsonBody<{
      keyId?: unknown;
      nonce?: unknown;
      signature?: unknown;
    }>(request);
    if (
      body === null ||
      typeof body.keyId !== "string" ||
      typeof body.nonce !== "string" ||
      typeof body.signature !== "string"
    ) {
      return jsonError(400, "keyId, nonce and signature required");
    }
    if (!this.auth.available) {
      return Response.json(
        { error: "operator authentication not configured" },
        { status: 503 },
      );
    }
    const result = await this.auth.login(
      body.keyId,
      body.nonce,
      body.signature,
      Date.now(),
    );
    if (!result.ok) return jsonError(401, "authentication failed");
    const maxAgeSec = Math.floor(SESSION_TTL_MS / 1000);
    return new Response(
      JSON.stringify({
        operator: result.session.operator,
        keyId: result.session.keyId,
        expiresAtEpochMs: result.session.expiresAtEpochMs,
      }),
      {
        status: 200,
        headers: {
          "content-type": "application/json",
          "set-cookie": sessionCookie(result.session.token, maxAgeSec),
        },
      },
    );
  }

  /** Revocation is server-side first: the session dies in the room whether or
   * not the client keeps the cookie. */
  private handleAuthLogout(request: Request): Response {
    if (request.method !== "POST") return jsonError(405, "method not allowed");
    if (!PositionRoom.consoleGuarded(request)) return jsonError(403, "missing console header");
    const token = readSessionToken(request);
    this.auth.logout(token);
    return new Response(JSON.stringify({ loggedOut: true }), {
      status: 200,
      headers: {
        "content-type": "application/json",
        "set-cookie": CLEAR_SESSION_COOKIE,
      },
    });
  }

  /** Whoami for the console UI. An expired session answers exactly like a
   * missing one — 401 — because from outside there is no difference. */
  private handleAuthStatus(request: Request): Response {
    if (request.method !== "GET") return jsonError(405, "method not allowed");
    const session = this.auth.validate(readSessionToken(request), Date.now());
    if (session === null) return jsonError(401, "authentication failed");
    return Response.json({
      operator: session.operator,
      keyId: session.keyId,
      expiresAtEpochMs: session.expiresAtEpochMs,
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
