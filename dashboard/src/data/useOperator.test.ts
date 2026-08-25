//! Unit tests for the operator transport's audit log (HJ-314): issuer
//! stamping from the send context on every outcome path, the un-audited
//! local in-flight guard, and the newest-first cap.

import { act, renderHook } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import { useOperator } from "./useOperator";

function stubFetch(
  impl: () => Promise<Response>,
): ReturnType<typeof vi.fn> {
  const fetchMock = vi.fn(impl);
  vi.stubGlobal("fetch", fetchMock as unknown as typeof fetch);
  return fetchMock;
}

describe("useOperator audit log", () => {
  afterEach(() => {
    vi.unstubAllGlobals();
  });

  it("stamps queued entries with the send-context issuer", async () => {
    stubFetch(() =>
      Promise.resolve(new Response(JSON.stringify({ queued: 7 }), { status: 202 })),
    );
    const { result } = renderHook(() => useOperator("https://test/command"));

    await act(async () => {
      await result.current.send({ t: "session-end" }, { issuer: "harlan" });
    });

    expect(result.current.auditLog).toHaveLength(1);
    expect(result.current.auditLog[0]).toMatchObject({
      id: 7,
      status: "queued",
      issuer: "harlan",
    });
  });

  it("stamps refused entries too — attribution survives refusal", async () => {
    stubFetch(() =>
      Promise.resolve(
        new Response(JSON.stringify({ error: "command failed schema validation" }), {
          status: 400,
        }),
      ),
    );
    const { result } = renderHook(() => useOperator("https://test/command"));

    await act(async () => {
      await result.current.send({ t: "session-end" }, { issuer: "harlan" });
    });

    expect(result.current.auditLog[0]).toMatchObject({
      id: null,
      status: "refused",
      error: "command failed schema validation",
      issuer: "harlan",
    });
  });

  it("leaves issuer undefined when no context is given — never invented", async () => {
    stubFetch(() =>
      Promise.resolve(new Response(JSON.stringify({ queued: 1 }), { status: 202 })),
    );
    const { result } = renderHook(() => useOperator("https://test/command"));

    await act(async () => {
      await result.current.send({ t: "session-end" });
    });

    expect(result.current.auditLog[0]!.issuer).toBeUndefined();
  });

  it("does not audit the local in-flight guard rejection", async () => {
    let release!: (response: Response) => void;
    stubFetch(
      () =>
        new Promise<Response>((resolve) => {
          release = resolve;
        }),
    );
    const { result } = renderHook(() => useOperator("https://test/command"));

    let first!: Promise<unknown>;
    act(() => {
      first = result.current.send({ t: "session-end" });
    });
    const guarded = await result.current.send({ t: "session-end" });
    expect(guarded.ok).toBe(false);
    expect(result.current.auditLog).toHaveLength(0);

    await act(async () => {
      release(new Response(JSON.stringify({ queued: 1 }), { status: 202 }));
      await first;
    });
    expect(result.current.auditLog).toHaveLength(1);
  });

  it("caps the log at 24 entries, newest first", async () => {
    stubFetch(() =>
      Promise.resolve(new Response(JSON.stringify({ queued: 0 }), { status: 202 })),
    );
    const { result } = renderHook(() => useOperator("https://test/command"));

    await act(async () => {
      for (let index = 0; index < 30; index += 1) {
        await result.current.send({ t: "session-end" });
      }
    });

    expect(result.current.auditLog).toHaveLength(24);
    // Newest first: the first entry in the log is the most recent send.
    expect(result.current.auditLog[0]!.sentAtMs).toBeGreaterThanOrEqual(
      result.current.auditLog[23]!.sentAtMs,
    );
  });
});
