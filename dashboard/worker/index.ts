//! Worker entrypoint for the arbkit dashboard.
//!
//! Static assets keep serving exactly as before (the SPA fallback included);
//! this script only intercepts the live-stream API surface and forwards it to
//! the single `PositionRoom` Durable Object. Ingest is the one authenticated
//! surface — viewers connect read-only, by design.

import { PositionRoom, type Env } from "./position-room";

export { PositionRoom };

const INGEST_PATH = "/api/live/ingest";
const WS_PATH = "/api/live/ws";

export default {
  async fetch(request: Request, env: Env): Promise<Response> {
    const url = new URL(request.url);

    if (url.pathname === INGEST_PATH) return handleIngest(request, env);
    if (url.pathname === WS_PATH) return forwardToRoom(env, request);

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
  const configured = env.LIVE_INGEST_TOKEN;
  if (!configured) {
    return Response.json(
      { error: "ingest token not configured (set LIVE_INGEST_TOKEN)" },
      { status: 503 },
    );
  }
  const presented = request.headers.get("authorization") ?? "";
  const expected = `Bearer ${configured}`;
  if (!timingSafeEqual(presented, expected)) {
    return Response.json({ error: "unauthorized" }, { status: 401 });
  }
  return forwardToRoom(env, request);
}

function forwardToRoom(env: Env, request: Request): Promise<Response> {
  const id = env.POSITION_ROOM.idFromName("live");
  return env.POSITION_ROOM.get(id).fetch(request);
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
