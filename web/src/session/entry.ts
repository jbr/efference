// Which binding served this page, and therefore what it shows first
// (docs/web-client.md). Three answers: a single session, a host's list of them,
// or the sign-in view when the page holds no bearer.
//
// The base decides it when there is one — a page under `/sessions/<id>/` is that
// session's view, and no request is needed to know it. At a root the base is
// empty and the binding is genuinely ambiguous, so the client asks: the host
// answers `GET /sessions` with the listing, a session daemon serves no such
// route and answers `404`, and a host with no credential yet answers `401` —
// which is the sign-in view. Nothing else distinguishes them — the shell is
// byte-identical — and the probe is one small same-origin request against the
// endpoint the page loaded from.
//
// Anything else (offline, a dropped connection) reads as a session, which is
// what a root has always been: the session view then reports the network state
// through its composer status rather than a second failure screen.

import { useEffect, useState, useSyncExternalStore } from "react";
import { apiBase } from "../api/base.ts";
import { bearer } from "../api/bearer.ts";
import { lastListing } from "./listing.ts";
import { ApiError, fetchSessions, type SessionEntry } from "../api/http.ts";

export type Entry =
  /** One session's view: the page is under a session prefix, or the root is a
   * session daemon's. */
  | { kind: "session" }
  /** The host's root: the live sessions, each a link into its own view. */
  | { kind: "list"; sessions: SessionEntry[] }
  /** No credential: the host's root answered the guarded listing with a `401`,
   * or the page holds no bearer. */
  | { kind: "unauthenticated" };

const SIGNED_OUT: Entry = { kind: "unauthenticated" };

export async function resolveEntry(): Promise<Entry> {
  if (apiBase() !== "") return { kind: "session" };
  try {
    return { kind: "list", sessions: await fetchSessions() };
  } catch (error) {
    if (error instanceof ApiError && error.status === 401) return SIGNED_OUT;
    return { kind: "session" };
  }
}

/** The binding, once known; `null` while the probe is out. While the page holds
 * no bearer it is `unauthenticated`, whatever the probe has answered: the
 * sign-in view takes the place of any other (ui/App.tsx), and a refused bearer
 * is cleared where it is refused (`apiFetch`, api/http.ts), so no view needs a
 * `401` branch of its own.
 *
 * `base` is the page's current base, which navigation changes without a load
 * (ui/route.ts): under a prefix the binding is a session's view and is known at
 * once; at the root the probe's last answer stays up while the next one is out,
 * so returning to the list from a session paints the list it left.
 *
 * The probe runs again whenever the bearer changes, signed out included. A host
 * answers a page with no bearer `401`, so at a sign-in the answer on hand is
 * `unauthenticated` and the sign-in view stays up until the probe under the new
 * bearer answers; an answer that arrives after the bearer has moved again is
 * dropped. */
export function useEntry(base: string): Entry | null {
  const held = useSyncExternalStore(bearer.subscribe, bearer.held);
  const [root, setRoot] = useState<Entry | null>(null);
  useEffect(() => {
    if (base !== "") return;
    let current = true;
    void resolveEntry().then((answer) => {
      if (current) setRoot(answer);
    });
    return () => {
      current = false;
    };
  }, [base, held]);
  if (held === null) return SIGNED_OUT;
  if (base !== "") return { kind: "session" };
  // Before any probe has answered, a listing some session's switcher heard is
  // as good a seed for the list as the probe's own.
  const heard = lastListing();
  const seeded: Entry | null =
    heard === null ? null : { kind: "list", sessions: heard };
  return root ?? seeded;
}
