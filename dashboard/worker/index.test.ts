//! Token-enforcement and routing tests for the worker entrypoint.
//!
//! Ingest and operator commands are the two authenticated surfaces, so these
//! pin both matrices against a stubbed room: 503 when no token is
//! configured, 401 on any wrong credential (including scheme and byte-level
//! mismatches), method gating before authentication, read-only public viewer
//! forwarding, and static-asset fallback. The room stub records what
//! actually reached the Durable Object — an unauthorized request must never
//! get that far.

import { describe, expect, it } from "vitest";

import worker from "./index";

type EnvShape = ConstructorParameters<(typeof worker)["fetch"]>[1];
type CtxShape = Parameters<(typeof worker)["fetch"]>[2];

const TOKEN = "secret-token";
const OPERATOR_TOKEN = "operator-token";
const INGEST_URL = "https://dashboard.example/api/live/ingest";
const WS_URL = "https://dashboard.example/api/live/ws";
const COMMAND_URL = "https://dashboard.example/api/live/command";
const RUNNER_COMMANDS_URL = "https://dashboard.example/api/live/commands";

interface EnvStub {
  env: EnvShape;
  roomRequests: Request[];
  assetRequests: Request[];
}

function makeEnv(tokens?: { ingest?: string; operator?: string }): EnvStub {
  const roomRequests: Request[] = [];
  const assetRequests: Request[] = [];
  const env = {
    LIVE_INGEST_TOKEN: tokens?.ingest,
    LIVE_OPERATOR_TOKEN: tokens?.operator,
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

async function postCommand(
  env: EnvShape,
  authorization?: string,
): Promise<Response> {
  const headers = new Headers({ "content-type": "application/json" });
  if (authorization !== undefined) headers.set("authorization", authorization);
  return worker.fetch(
    new Request(COMMAND_URL, {
      method: "POST",
      headers,
      body: JSON.stringify({ t: "kill-switch", engage: false, confirm: true }),
    }),
    env,
    {} as CtxShape,
  );
}

describe("worker ingest token enforcement", () => {
  it("forwards correctly authenticated batches to the room", async () => {
    const { env, roomRequests, assetRequests } = makeEnv({ ingest: TOKEN });

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
      const { env, roomRequests } = makeEnv({ ingest: TOKEN });

      const response = await postIngest(env, presented);

      expect(response.status, `credentials: ${String(presented)}`).toBe(401);
      expect(await response.json(), `credentials: ${String(presented)}`).toMatchObject({
        error: "unauthorized",
      });
      expect(roomRequests, `credentials: ${String(presented)}`).toHaveLength(0);
    }
  });

  it("refuses ingest with 503 while no token is configured", async () => {
    const { env, roomRequests } = makeEnv({});

    const response = await postIngest(env, `Bearer ${TOKEN}`);

    expect(response.status).toBe(503);
    expect(await response.json()).toMatchObject({
      error: expect.stringContaining("LIVE_INGEST_TOKEN"),
    });
    expect(roomRequests).toHaveLength(0);
  });

  it("gates the method before spending the credential check", async () => {
    const { env, roomRequests } = makeEnv({ ingest: TOKEN });

    const response = await worker.fetch(
      new Request(INGEST_URL), // GET, no authorization header at all
      env,
      {} as CtxShape,
    );

    expect(response.status).toBe(405);
    expect(roomRequests).toHaveLength(0);
  });
});

describe("worker operator command enforcement", () => {
  it("forwards correctly authenticated commands to the room", async () => {
    const { env, roomRequests } = makeEnv({ operator: OPERATOR_TOKEN });

    const response = await postCommand(env, `Bearer ${OPERATOR_TOKEN}`);

    expect(response.status).toBe(204);
    expect(roomRequests).toHaveLength(1);
    expect(new URL(roomRequests[0]!.url).pathname).toBe("/api/live/command");
  });

  it("answers 401 without reaching the room on missing or wrong credentials", async () => {
    for (const presented of [
      undefined,
      "",
      "Basic b3BlcmF0b3ItdG9rZW4",
      "Bearer wrong-operator-token",
      `bearer ${OPERATOR_TOKEN}`,
    ]) {
      const { env, roomRequests } = makeEnv({ operator: OPERATOR_TOKEN });

      const response = await postCommand(env, presented);

      expect(response.status, `credentials: ${String(presented)}`).toBe(401);
      expect(await response.json()).toMatchObject({ error: "unauthorized" });
      expect(roomRequests, `credentials: ${String(presented)}`).toHaveLength(0);
    }
  });

  it("never accepts the ingest token as operator authority", async () => {
    // Holding the runner's push credential must not confer the right to
    // command: two surfaces, two secrets.
    const { env, roomRequests } = makeEnv({ ingest: TOKEN, operator: OPERATOR_TOKEN });

    const response = await postCommand(env, `Bearer ${TOKEN}`);

    expect(response.status).toBe(401);
    expect(roomRequests).toHaveLength(0);
  });

  it("refuses commands with 503 while no operator token is configured", async () => {
    const { env, roomRequests } = makeEnv({});

    const response = await postCommand(env, `Bearer ${OPERATOR_TOKEN}`);

    expect(response.status).toBe(503);
    expect(await response.json()).toMatchObject({
      error: expect.stringContaining("LIVE_OPERATOR_TOKEN"),
    });
    expect(roomRequests).toHaveLength(0);
  });

  it("gates the method before spending the credential check", async () => {
    const { env, roomRequests } = makeEnv({ operator: OPERATOR_TOKEN });

    const response = await worker.fetch(new Request(COMMAND_URL), env, {} as CtxShape);

    expect(response.status).toBe(405);
    expect(roomRequests).toHaveLength(0);
  });
});

describe("worker runner command-pull enforcement", () => {
  it("serves the pull endpoint only with a valid ingest credential", async () => {
    const { env, roomRequests } = makeEnv({ ingest: TOKEN });

    const ok = await worker.fetch(
      new Request(`${RUNNER_COMMANDS_URL}?afterId=3`, {
        headers: { authorization: `Bearer ${TOKEN}` },
      }),
      env,
      {} as CtxShape,
    );
    expect(ok.status).toBe(204);

    const unauthorized = await worker.fetch(
      new Request(RUNNER_COMMANDS_URL, { headers: { authorization: "Bearer nope" } }),
      env,
      {} as CtxShape,
    );
    expect(unauthorized.status).toBe(401);
    expect(
      (await worker.fetch(new Request(RUNNER_COMMANDS_URL), env, {} as CtxShape)).status,
    ).toBe(401);
    expect(roomRequests).toHaveLength(1);
  });

  it("answers GET-only on the pull endpoint", async () => {
    const { env } = makeEnv({ ingest: TOKEN });

    const response = await worker.fetch(
      new Request(RUNNER_COMMANDS_URL, { method: "POST" }),
      env,
      {} as CtxShape,
    );
    expect(response.status).toBe(405);
  });
});

describe("worker routing", () => {
  it("forwards viewer websocket upgrades to the room unauthenticated", async () => {
    const { env, roomRequests } = makeEnv({ ingest: TOKEN });

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
    const { env, roomRequests, assetRequests } = makeEnv({ ingest: TOKEN });

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
