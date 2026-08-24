//! Token-enforcement and routing tests for the worker entrypoint.
//!
//! Ingest is the one authenticated surface, so these pin the whole matrix
//! against a stubbed room: 503 when no token is configured, 401 on any
//! wrong credential (including scheme and byte-level mismatches), method
//! gating before authentication, read-only public viewer forwarding, and
//! static-asset fallback. The room stub records what actually reached the
//! Durable Object — an unauthorized request must never get that far.

import { describe, expect, it } from "vitest";

import worker from "./index";

type EnvShape = ConstructorParameters<(typeof worker)["fetch"]>[1];
type CtxShape = Parameters<(typeof worker)["fetch"]>[2];

const TOKEN = "secret-token";
const INGEST_URL = "https://dashboard.example/api/live/ingest";
const WS_URL = "https://dashboard.example/api/live/ws";

interface EnvStub {
  env: EnvShape;
  roomRequests: Request[];
  assetRequests: Request[];
}

function makeEnv(token?: string): EnvStub {
  const roomRequests: Request[] = [];
  const assetRequests: Request[] = [];
  const env = {
    LIVE_INGEST_TOKEN: token,
    POSITION_ROOM: {
      idFromName: (name: string) => ({ name }),
      get: () => ({
        fetch: async (request: Request) => {
          roomRequests.push(request);
          return new Response(null, { status: 204 });
        },
      }),
    },
    ASSETS: {
      fetch: async (request: Request) => {
        assetRequests.push(request);
        return new Response("asset");
      },
    },
  } as unknown as EnvShape;
  return { env, roomRequests, assetRequests };
}

async function postIngest(env: EnvShape, authorization?: string): Promise<Response> {
  const headers = new Headers({ "content-type": "application/x-ndjson" });
  if (authorization !== undefined) headers.set("authorization", authorization);
  return worker.fetch(
    new Request(INGEST_URL, { method: "POST", headers }),
    env,
    {} as CtxShape,
  );
}

describe("worker ingest token enforcement", () => {
  it("forwards correctly authenticated batches to the room", async () => {
    const { env, roomRequests, assetRequests } = makeEnv(TOKEN);

    const response = await postIngest(env, `Bearer ${TOKEN}`);

    expect(response.status).toBe(204);
    expect(roomRequests).toHaveLength(1);
    expect(new URL(roomRequests[0]!.url).pathname).toBe("/api/live/ingest");
    expect(assetRequests).toHaveLength(0);
  });

  it("answers 401 without reaching the room on missing or wrong credentials", async () => {
    for (const presented of [
      undefined,
      "",
      "Basic c2VjcmV0LXRva2Vu",
      "Bearer wrong-token", // equal length, differing bytes
      `bearer ${TOKEN}`, // right bytes, wrong scheme
      "Bearer ", // scheme only, empty credential
    ]) {
      const { env, roomRequests } = makeEnv(TOKEN);

      const response = await postIngest(env, presented);

      expect(response.status, `credentials: ${String(presented)}`).toBe(401);
      expect(await response.json(), `credentials: ${String(presented)}`).toMatchObject({
        error: "unauthorized",
      });
      expect(roomRequests, `credentials: ${String(presented)}`).toHaveLength(0);
    }
  });

  it("refuses ingest with 503 while no token is configured", async () => {
    const { env, roomRequests } = makeEnv(undefined);

    const response = await postIngest(env, `Bearer ${TOKEN}`);

    expect(response.status).toBe(503);
    expect(await response.json()).toMatchObject({
      error: expect.stringContaining("LIVE_INGEST_TOKEN"),
    });
    expect(roomRequests).toHaveLength(0);
  });

  it("gates the method before spending the credential check", async () => {
    const { env, roomRequests } = makeEnv(TOKEN);

    const response = await worker.fetch(
      new Request(INGEST_URL), // GET, no authorization header at all
      env,
      {} as CtxShape,
    );

    expect(response.status).toBe(405);
    expect(roomRequests).toHaveLength(0);
  });
});

describe("worker routing", () => {
  it("forwards viewer websocket upgrades to the room unauthenticated", async () => {
    const { env, roomRequests } = makeEnv(TOKEN);

    const response = await worker.fetch(
      new Request(WS_URL, { headers: { upgrade: "websocket" } }),
      env,
      {} as CtxShape,
    );

    // The stubbed room answers 204; what matters is the request arrived.
    expect(response.status).toBe(204);
    expect(roomRequests).toHaveLength(1);
  });

  it("serves everything else from static assets", async () => {
    const { env, roomRequests, assetRequests } = makeEnv(TOKEN);

    for (const path of ["/", "/proof-ledger", "/data/runs/index.json"]) {
      const response = await worker.fetch(
        new Request(`https://dashboard.example${path}`),
        env,
        {} as CtxShape,
      );

      expect(response.status, path).toBe(200);
      expect(await response.text(), path).toBe("asset");
    }
    expect(roomRequests).toHaveLength(0);
    expect(assetRequests).toHaveLength(3);
  });
});
