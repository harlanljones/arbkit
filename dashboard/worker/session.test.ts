//! Unit tests for the operator session machinery (HJ-310).
//!
//! Drives `OperatorAuth` directly over WebCrypto-generated key pairs: roster
//! parsing fail-closed rules, challenge single-use and expiry, signature
//! verification against the frozen preimage, one-active-session-per-operator,
//! logout, and lazy session expiry. HTTP-level flows live in
//! `position-room.test.ts`.

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import {
  CHALLENGE_TTL_MS,
  CLEAR_SESSION_COOKIE,
  CONSOLE_HEADER,
  loginPreimage,
  OperatorAuth,
  parseRoster,
  SESSION_COOKIE,
  SESSION_TTL_MS,
  sessionCookie,
} from "./session";

const enc = new TextEncoder();

async function makeKeyPair(): Promise<{ publicKeyPem: string; sign(data: string): Promise<string> }> {
  const pair = await crypto.subtle.generateKey(
    {
      name: "RSA-PSS",
      modulusLength: 2048,
      publicExponent: new Uint8Array([1, 0, 1]),
      hash: "SHA-256",
    },
    true,
    ["sign", "verify"],
  );
  const spki = await crypto.subtle.exportKey("spki", pair.publicKey);
  const body = btoa(String.fromCharCode(...new Uint8Array(spki)));
  const publicKeyPem = [
    "-----BEGIN PUBLIC KEY-----",
    ...(body.match(/.{1,64}/g) ?? []),
    "-----END PUBLIC KEY-----",
  ].join("\n");
  return {
    publicKeyPem,
    async sign(data: string) {
      const signature = await crypto.subtle.sign(
        { name: "RSA-PSS", saltLength: 32 },
        pair.privateKey,
        enc.encode(data),
      );
      return btoa(String.fromCharCode(...new Uint8Array(signature)));
    },
  };
}

function rosterFor(keyId: string, pem: string): string {
  return JSON.stringify([{ keyId, name: "harlan", publicKeyPem: pem }]);
}

describe("roster parsing", () => {
  it("fails closed on absent, empty, malformed, or private-key rosters", () => {
    expect(parseRoster(undefined)).toBeNull();
    expect(parseRoster("")).toBeNull();
    expect(parseRoster("   ")).toBeNull();
    expect(parseRoster("{not json")).toBeNull();
    expect(parseRoster("[]")).toBeNull();
    // A pasted PRIVATE key must never become a roster entry.
    expect(
      parseRoster(
        JSON.stringify([
          { keyId: "k1", name: "harlan", publicKeyPem: "-----BEGIN PRIVATE KEY-----x" },
        ]),
      ),
    ).toBeNull();
    expect(
      parseRoster(JSON.stringify([{ keyId: "", name: "harlan", publicKeyPem: "-----BEGIN PUBLIC KEY-----x" }])),
    ).toBeNull();
  });
});

describe("OperatorAuth", () => {
  let operator: Awaited<ReturnType<typeof makeKeyPair>>;
  const KEY_ID = "test-key-id-0001";

  beforeEach(async () => {
    vi.useFakeTimers();
    vi.setSystemTime(1_000_000);
    operator = await makeKeyPair();
  });
  afterEach(() => {
    vi.useRealTimers();
  });

  function authWith(pem: string = operator.publicKeyPem): OperatorAuth {
    return new OperatorAuth(parseRoster(rosterFor(KEY_ID, pem)));
  }

  async function loginFlow(auth: OperatorAuth) {
    const challenge = auth.issueChallenge(KEY_ID, Date.now());
    if (challenge === null) throw new Error("challenge unavailable");
    const signature = await operator.sign(
      loginPreimage(KEY_ID, challenge.nonce, challenge.issuedAtMs),
    );
    return auth.login(KEY_ID, challenge.nonce, signature, Date.now());
  }

  it("reports unavailable without a usable roster and refuses everything", async () => {
    const auth = new OperatorAuth(null);
    expect(auth.available).toBe(false);
    expect(auth.issueChallenge(KEY_ID, Date.now())).toBeNull();
    expect((await auth.login(KEY_ID, "n", "s", Date.now())).ok).toBe(false);
    expect(new OperatorAuth(parseRoster("[]")).available).toBe(false);
  });

  it("issues a working login from a valid challenge + signature", async () => {
    const auth = authWith();
    const result = await loginFlow(auth);
    expect(result.ok).toBe(true);
    if (!result.ok) return;
    expect(result.session.operator).toBe("harlan");
    expect(result.session.keyId).toBe(KEY_ID);
    expect(result.session.expiresAtEpochMs).toBe(Date.now() + SESSION_TTL_MS);
    expect(auth.validate(result.session.token, Date.now())).not.toBeNull();
  });

  it("burns the nonce on any login attempt — single-use", async () => {
    const auth = authWith();
    const challenge = auth.issueChallenge(KEY_ID, Date.now())!;
    const goodSignature = await operator.sign(
      loginPreimage(KEY_ID, challenge.nonce, challenge.issuedAtMs),
    );
    // A wrong-key attempt consumes the nonce…
    expect((await auth.login("other-key", challenge.nonce, goodSignature, Date.now())).ok).toBe(
      false,
    );
    // …so the correct replay afterwards fails too.
    expect((await auth.login(KEY_ID, challenge.nonce, goodSignature, Date.now())).ok).toBe(false);
  });

  it("rejects expired challenges, wrong signatures, and tampered preimages", async () => {
    const auth = authWith();

    const stale = auth.issueChallenge(KEY_ID, Date.now())!;
    vi.advanceTimersByTime(CHALLENGE_TTL_MS + 1);
    expect(
      (
        await auth.login(
          KEY_ID,
          stale.nonce,
          await operator.sign(loginPreimage(KEY_ID, stale.nonce, stale.issuedAtMs)),
          Date.now(),
        )
      ).ok,
    ).toBe(false);

    const challenge = auth.issueChallenge(KEY_ID, Date.now())!;
    const signedForOtherNonce = await operator.sign(
      loginPreimage(KEY_ID, "a-different-nonce", challenge.issuedAtMs),
    );
    expect((await auth.login(KEY_ID, challenge.nonce, signedForOtherNonce, Date.now())).ok).toBe(
      false,
    );

    const third = auth.issueChallenge(KEY_ID, Date.now())!;
    const badTimestamp = await operator.sign(
      ["arbkit-dashboard-login", KEY_ID, third.nonce, String(third.issuedAtMs + 1)].join("\n"),
    );
    expect((await auth.login(KEY_ID, third.nonce, badTimestamp, Date.now())).ok).toBe(false);
  });

  it("answers unknown key ids with null challenges, not roster enumeration", () => {
    const auth = authWith();
    expect(auth.issueChallenge("who-is-this", Date.now())).toBeNull();
  });

  it("keeps exactly one active session per operator", async () => {
    const auth = authWith();
    const first = await loginFlow(auth);
    const second = await loginFlow(auth);
    if (!first.ok || !second.ok) throw new Error("logins should succeed");
    expect(first.session.token).not.toBe(second.session.token);
    expect(auth.activeSessionCount()).toBe(1);
    // The retired token no longer validates.
    expect(auth.validate(first.session.token, Date.now())).toBeNull();
    expect(auth.validate(second.session.token, Date.now())).not.toBeNull();
  });

  it("expires sessions lazily and drops them on read", async () => {
    const auth = authWith();
    const result = await loginFlow(auth);
    if (!result.ok) throw new Error("login should succeed");
    expect(auth.validate(result.session.token, Date.now() + SESSION_TTL_MS - 1)).not.toBeNull();
    expect(auth.validate(result.session.token, Date.now() + SESSION_TTL_MS)).toBeNull();
    expect(auth.activeSessionCount()).toBe(0);
  });

  it("logs out server-side; the cookie alone proves nothing afterwards", async () => {
    const auth = authWith();
    const result = await loginFlow(auth);
    if (!result.ok) throw new Error("login should succeed");
    expect(auth.logout(result.session.token)).toBe(true);
    expect(auth.validate(result.session.token, Date.now())).toBeNull();
    expect(auth.logout(result.session.token)).toBe(false);
    expect(auth.logout(null)).toBe(false);
  });

  it("rejects a validly-signed preimage from a key that is not on the roster", async () => {
    const impostor = await makeKeyPair();
    const auth = new OperatorAuth(parseRoster(rosterFor(KEY_ID, operator.publicKeyPem)));
    const challenge = auth.issueChallenge(KEY_ID, Date.now())!;
    const forgedSignature = await impostor.sign(
      loginPreimage(KEY_ID, challenge.nonce, challenge.issuedAtMs),
    );
    expect((await auth.login(KEY_ID, challenge.nonce, forgedSignature, Date.now())).ok).toBe(
      false,
    );
  });

  it("revokes by roster removal: a recycled room honors neither old sessions nor re-login", async () => {
    // Session state lives per isolate, and the roster is fixed at
    // construction — so rotating the roster secret recycles the worker,
    // which drops every session AND removes the operator's ability to log
    // back in. This pins that contract from both sides.
    const secondOperator = await makeKeyPair();
    const bothRoster = JSON.stringify([
      { keyId: KEY_ID, name: "harlan", publicKeyPem: operator.publicKeyPem },
      {
        keyId: "second-key-id-0002",
        name: "backup",
        publicKeyPem: secondOperator.publicKeyPem,
      },
    ]);
    const before = new OperatorAuth(parseRoster(bothRoster));
    const result = await loginFlow(before);
    if (!result.ok) throw new Error("login should succeed");
    expect(before.validate(result.session.token, Date.now())).not.toBeNull();

    // The operator is removed from the roster; the secret update recycles
    // the room into this replacement instance.
    const after = new OperatorAuth(
      parseRoster(rosterFor("second-key-id-0002", secondOperator.publicKeyPem)),
    );
    // Old sessions do not survive the recycle…
    expect(after.validate(result.session.token, Date.now())).toBeNull();
    // …and the revoked operator cannot re-login…
    expect(after.issueChallenge(KEY_ID, Date.now())).toBeNull();
    // …while everyone still on the roster can.
    expect(after.issueChallenge("second-key-id-0002", Date.now())).not.toBeNull();
    expect(after.available).toBe(true);
  });
});

describe("cookie contract", () => {
  it("sets HttpOnly, Secure, SameSite=Strict with a bounded Max-Age", () => {
    const cookie = sessionCookie("abc123", 3600);
    expect(cookie).toContain(`${SESSION_COOKIE}=abc123`);
    expect(cookie).toContain("HttpOnly");
    expect(cookie).toContain("Secure");
    expect(cookie).toContain("SameSite=Strict");
    expect(cookie).toContain("Path=/");
    expect(cookie).toContain("Max-Age=3600");

    expect(CLEAR_SESSION_COOKIE).toContain(`${SESSION_COOKIE}=;`);
    expect(CLEAR_SESSION_COOKIE).toContain("Max-Age=0");
    expect(CONSOLE_HEADER).toBe("x-arbkit-console");
  });
});
