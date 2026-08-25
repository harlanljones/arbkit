//! Worker entrypoint for the arbkit dashboard.
//!
//! Static assets keep serving exactly as before (the SPA fallback included);
//! this script only intercepts the live-stream API surface and forwards it to
//! the single `PositionRoom` Durable Object. Two of the three live surfaces
//! are authenticated: runner ingest (`LIVE_INGEST_TOKEN`) and operator
//! commands (`LIVE_OPERATOR_TOKEN`, distinct from ingest by design). Viewers
//! connect read-only, by design.

import { PositionRoom, type Env } from "./position-room";

export { PositionRoom };

const INGEST_PATH = "/api/live/ingest";
const WS_PATH = "/api/live/ws";
const COMMAND_PATH = "/api/live/command";
const RUNNER_COMMANDS_PATH = "/api/live/commands";
// Operator authentication surfaces. These carry their own auth semantics
// (challenge/login/logout/session) and are forwarded to the room ungated:
// the room answers 503 fail-closed when no operator roster is configured.
const AUTH_CHALLENGE_PATH = "/api/live/auth/challenge";
const AUTH_LOGIN_PATH = "/api/live/auth/login";
const AUTH_LOGOUT_PATH = "/api/live/auth/logout";
const AUTH_SESSION_PATH = "/api/live/auth/session";

export default {
  async fetch(request: Request, env: Env): Promise<Response> {
    const url = new URL(request.url);

    if (url.pathname === INGEST_PATH) return handleIngest(request, env);
    if (url.pathname === WS_PATH) return forwardToRoom(env, request);
    if (url.pathname === COMMAND_PATH) return forwardToRoom(env, request);
    if (url.pathname === RUNNER_COMMANDS_PATH) return handleRunnerCommands(request, env);
    if (
      url.pathname === AUTH_CHALLENGE_PATH ||
      url.pathname === AUTH_LOGIN_PATH ||
      url.pathname === AUTH_LOGOUT_PATH ||
      url.pathname === AUTH_SESSION_PATH
    ) {
      return forwardToRoom(env, request);
    }

    // Everything else is the static SPA — including its client-side routes,
    // which `not_found_handling: "single-page-application"` covers through
    // the ASSETS binding.
    return env.ASSETS.fetch(request);
  },
} satisfies ExportedHandler<Env>;

async function handleIngest(request: Request, env: Env): Promise<Response> {
  if (request.method !== "POST") {
    return Response.json({ error: "method not allowed" }, { status: 405 });
  }
  const gate = requireBearer(request, env.LIVE_INGEST_TOKEN, "LIVE_INGEST_TOKEN");
  if (gate !== null) return gate;
  return forwardToRoom(env, request);
}

// The operator command surface is authenticated inside the room (HJ-311):
// session validation needs the room's live session store, so the worker
// edge forwards every command request and the room answers 401/503 itself.
// The two-secrets rule is unchanged — LIVE_INGEST_TOKEN gates only ingest +
// runner pulls here, and never confers authority to command.

/** The runner pulls its queued commands with the same credential it pushes
 * ingest with; commands are delivered to it, not accepted from it. */
async function handleRunnerCommands(request: Request, env: Env): Promise<Response> {
  if (request.method !== "GET") {
    return Response.json({ error: "method not allowed" }, { status: 405 });
  }
  const gate = requireBearer(request, env.LIVE_INGEST_TOKEN, "LIVE_INGEST_TOKEN");
  if (gate !== null) return gate;
  return forwardToRoom(env, request);
}

function forwardToRoom(env: Env, request: Request): Promise<Response> {
  const id = env.POSITION_ROOM.idFromName("live");
  return env.POSITION_ROOM.get(id).fetch(request);
}

/** Returns a response to send when the credential check fails, or `null` to
 * proceed. A missing configuration is a 503 — fail closed, never open. */
function requireBearer(
  request: Request,
  configured: string | undefined,
  variableName: string,
): Response | null {
  if (!configured) {
    return Response.json(
      { error: `${variableName} not configured (set ${variableName})` },
      { status: 503 },
    );
  }
  const presented = request.headers.get("authorization") ?? "";
  const expected = `Bearer ${configured}`;
  if (!timingSafeEqual(presented, expected)) {
    return Response.json({ error: "unauthorized" }, { status: 401 });
  }
  return null;
}

/** Length-then-byte comparison. Not load-bearing against a timing oracle on
 * a POC ingest, but it costs nothing to not leak the prefix. */
function timingSafeEqual(presented: string, expected: string): boolean {
  if (presented.length !== expected.length) return false;
  let mismatch = 0;
  for (let index = 0; index < expected.length; index += 1) {
    mismatch |= presented.charCodeAt(index) ^ expected.charCodeAt(index);
  }
  return mismatch === 0;
}
