//! Operator session machinery: signed-challenge login per the AUTH
//! mechanism decision (HJ-309).
//!
//! Kalshi exposes no identity-provider surface, so "login with Kalshi" here
//! means possession proof: an operator's public key is provisioned out-of-band
//! into a roster; login issues a single-use nonce challenge whose preimage the
//! operator signs locally with their private key (RSA-PSS SHA-256 — the same
//! primitive family the runner's feed signer uses); this module verifies and,
//! on success, issues a short-lived revocable session carried in an httpOnly
//! cookie. Private keys never touch the worker.
//!
//! Honesty rules carried over from the rest of the worker:
//! - Fail closed. A missing or malformed roster refuses all authentication
//!   (503) rather than degrading to anonymous authority.
//! - Uniform 401 for every credential failure (unknown key, stale nonce,
//!   wrong signature, expired session) so responses never enumerate the
//!   roster or distinguish failure causes to an attacker.
//! - One active session per operator: a fresh login silently retires the
//!   previous one.

import { z } from "zod";

/** Login challenges die after this long; a nonce is single-use regardless. */
export const CHALLENGE_TTL_MS = 120_000;
/** Sessions are short-lived by design; renewal is re-login. */
export const SESSION_TTL_MS = 3_600_000;
export const SESSION_COOKIE = "arbkit_session";
/** CSRF defense-in-depth: every auth mutation must carry this custom header,
 * which cross-site forms cannot send without a CORS preflight. Combined with
 * SameSite=Strict cookies, cross-origin command forgery has no path. */
export const CONSOLE_HEADER = "x-arbkit-console";

export interface RosterEntry {
  keyId: string;
  name: string;
  publicKeyPem: string;
}

const rosterEntrySchema = z.object({
  keyId: z.string().min(1),
  name: z.string().min(1),
  // SPKI public keys only. A pasted PRIVATE key must fail closed here, not
  // become a roster entry that can never verify.
  publicKeyPem: z.string().startsWith("-----BEGIN PUBLIC KEY-----"),
});

const rosterSchema = z.array(rosterEntrySchema);

/** Parses the raw roster secret. Returns `null` for anything that must fail
 * closed — absent, malformed, or empty (an empty roster can authenticate
 * nobody, so it refuses service like every other unusable configuration). */
export function parseRoster(raw: string | undefined): RosterEntry[] | null {
  if (raw === undefined || raw.trim() === "") return null;
  try {
    const parsed = rosterSchema.safeParse(JSON.parse(raw));
    return parsed.success && parsed.data.length > 0 ? parsed.data : null;
  } catch {
    return null;
  }
}

/** The exact bytes an operator signs: newline-separated so no field can run
 * into another, versioned by prefix. Frozen contract between the console
 * tooling and this verifier — changing it invalidates every client. */
export function loginPreimage(keyId: string, nonce: string, issuedAtMs: number): string {
  return ["arbkit-dashboard-login", keyId, nonce, String(issuedAtMs)].join("\n");
}

interface Challenge {
  keyId: string;
  issuedAtMs: number;
  expiresAtMs: number;
}

export interface IssuedSession {
  token: string;
  operator: string;
  keyId: string;
  expiresAtEpochMs: number;
}

type LoginResult =
  | { ok: true; session: IssuedSession }
  | { ok: false };

function randomToken(): string {
  const bytes = new Uint8Array(32);
  crypto.getRandomValues(bytes);
  return Array.from(bytes, (b) => b.toString(16).padStart(2, "0")).join("");
}

function randomNonce(): string {
  const bytes = new Uint8Array(32);
  crypto.getRandomValues(bytes);
  return btoa(String.fromCharCode(...bytes));
}

function base64ToBytes(value: string): Uint8Array {
  const normalized = value.trim();
  const binary = atob(normalized);
  return Uint8Array.from(binary, (c) => c.charCodeAt(0));
}

/** Strips PEM armor and whitespace down to the DER bytes. */
function pemToDer(pem: string): Uint8Array {
  const body = pem
    .replace(/-----BEGIN [^-]+-----/g, "")
    .replace(/-----END [^-]+-----/g, "")
    .replace(/\s+/g, "");
  return base64ToBytes(body);
}

export class OperatorAuth {
  private readonly challenges = new Map<string, Challenge>();
  private readonly sessions = new Map<string, IssuedSession>();
  private readonly verifiedKeys = new Map<string, CryptoKey>();

  constructor(private readonly roster: RosterEntry[] | null) {}

  /** Fail-closed gate: without a usable roster nobody can authenticate. */
  get available(): boolean {
    return this.roster !== null && this.roster.length > 0;
  }

  /** Issues a single-use challenge bound to one roster key. Unknown key ids
   * return null so the caller answers with the same uniform 401 as a bad
   * signature — responses never enumerate the roster. */
  issueChallenge(
    keyId: string,
    now: number,
  ): { nonce: string; issuedAtMs: number; expiresAtMs: number } | null {
    if (!this.available || !this.roster!.some((entry) => entry.keyId === keyId)) {
      return null;
    }
    this.purgeExpired(now);
    const issuedAtMs = now;
    const expiresAtMs = now + CHALLENGE_TTL_MS;
    const nonce = randomNonce();
    this.challenges.set(nonce, { keyId, issuedAtMs, expiresAtMs });
    return { nonce, issuedAtMs, expiresAtMs };
  }

  async login(
    keyId: string,
    nonce: string,
    signatureB64: string,
    now: number,
  ): Promise<LoginResult> {
    if (!this.available) return { ok: false };
    // Single-use: the challenge is consumed before verification, so a failed
    // guess burns the nonce instead of leaving it open to brute force.
    const challenge = this.challenges.get(nonce);
    this.challenges.delete(nonce);
    if (
      challenge === undefined ||
      challenge.keyId !== keyId ||
      now >= challenge.expiresAtMs
    ) {
      return { ok: false };
    }
    const entry = this.roster!.find((candidate) => candidate.keyId === keyId);
    if (entry === undefined) return { ok: false };

    const data = new TextEncoder().encode(loginPreimage(keyId, nonce, challenge.issuedAtMs));
    let signature: Uint8Array;
    try {
      signature = base64ToBytes(signatureB64);
    } catch {
      return { ok: false };
    }
    const key = await this.verifiedKeyFor(entry);
    if (key === null) return { ok: false };
    const valid = await crypto.subtle.verify(
      { name: "RSA-PSS", saltLength: 32 },
      key,
      signature,
      data,
    );
    if (!valid) return { ok: false };

    // One active session per operator: issuing retires the previous one.
    for (const [existing, session] of this.sessions) {
      if (session.keyId === entry.keyId) this.sessions.delete(existing);
    }
    const session: IssuedSession = {
      token: randomToken(),
      operator: entry.name,
      keyId: entry.keyId,
      expiresAtEpochMs: now + SESSION_TTL_MS,
    };
    this.sessions.set(session.token, session);
    return { ok: true, session };
  }

  /** Returns the live session for a presented token, or null when missing or
   * expired (the expiry instant itself is exclusive — JWT `exp` semantics).
   * Expired entries are dropped lazily on read. */
  validate(token: string | null, now: number): IssuedSession | null {
    if (token === null) return null;
    const session = this.sessions.get(token);
    if (session === undefined) return null;
    if (now >= session.expiresAtEpochMs) {
      this.sessions.delete(token);
      return null;
    }
    return session;
  }

  logout(token: string | null): boolean {
    if (token === null) return false;
    return this.sessions.delete(token);
  }

  activeSessionCount(): number {
    return this.sessions.size;
  }

  private async verifiedKeyFor(entry: RosterEntry): Promise<CryptoKey | null> {
    const cached = this.verifiedKeys.get(entry.keyId);
    if (cached !== undefined) return cached;
    try {
      const key = await crypto.subtle.importKey(
        "spki",
        pemToDer(entry.publicKeyPem),
        { name: "RSA-PSS", hash: "SHA-256" },
        false,
        ["verify"],
      );
      this.verifiedKeys.set(entry.keyId, key);
      return key;
    } catch {
      // An unparseable roster key fails this login only; the roster itself
      // was already validated structurally at parse time.
      return null;
    }
  }

  private purgeExpired(now: number): void {
    for (const [nonce, challenge] of this.challenges) {
      if (now >= challenge.expiresAtMs) this.challenges.delete(nonce);
    }
  }
}

export function sessionCookie(token: string, maxAgeSec: number): string {
  return `${SESSION_COOKIE}=${token}; HttpOnly; Secure; SameSite=Strict; Path=/; Max-Age=${maxAgeSec}`;
}

export const CLEAR_SESSION_COOKIE =
  `${SESSION_COOKIE}=; HttpOnly; Secure; SameSite=Strict; Path=/; Max-Age=0`;

export function readSessionToken(request: Request): string | null {
  const header = request.headers.get("cookie");
  if (header === null) return null;
  for (const pair of header.split(";")) {
    const [name, ...rest] = pair.trim().split("=");
    if (name === SESSION_COOKIE && rest.length > 0) {
      return rest.join("=");
    }
  }
  return null;
}
