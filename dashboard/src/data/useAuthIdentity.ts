//! Who is this console? One whoami fetch on mount against the worker's
//! session endpoint.
//!
//! The answer comes from the room's session store via the httpOnly cookie,
//! so a returned name is the worker's own attestation — the same value it
//! stamps onto queued commands. Null means unauthenticated, unconfigured, or
//! unreachable: all render honestly as "—" in the audit trail rather than
//! inventing an attribution.

import { useEffect, useState } from "react";

export interface AuthIdentity {
  /** Worker-attested operator name for this console's session cookie, or
   * null when there is none. */
  operator: string | null;
}

function defaultSessionUrl(): string {
  if (typeof window === "undefined") return "/api/live/auth/session";
  return `${window.location.origin}/api/live/auth/session`;
}

export function useAuthIdentity(url?: string): AuthIdentity {
  const [operator, setOperator] = useState<string | null>(null);
  const endpoint = url ?? defaultSessionUrl();

  useEffect(() => {
    let disposed = false;
    fetch(endpoint)
      .then(async (response) => {
        if (!response.ok) return;
        const body = (await response.json().catch(() => null)) as
          | { operator?: unknown }
          | null;
        if (!disposed && body !== null && typeof body.operator === "string") {
          setOperator(body.operator);
        }
      })
      .catch(() => {
        // Fails inert: an unreachable auth surface leaves this console
        // unattributed, which is exactly what the trail will show.
      });
    return () => {
      disposed = true;
    };
  }, [endpoint]);

  return { operator };
}
