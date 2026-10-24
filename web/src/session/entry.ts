// Which binding served this page, and therefore what it shows first
// (docs/web-client.md). Two answers: a single session, or a host's list of them.
//
// The base decides it when there is one — a page under `/sessions/<id>/` is that
// session's view, and no request is needed to know it. At a root the base is
// empty and the binding is genuinely ambiguous, so the client asks: the host
// answers `GET /sessions` with the listing, a session daemon serves no such
// route and answers `404`, and a host with no credential yet answers `401` —
// which also clears the held bearer (api/bearer.ts), so the sign-in view starts
// from none. Nothing else distinguishes them — the shell is byte-identical —
// and the probe is one small same-origin request against the endpoint the page
// loaded from.
//
// Anything else (offline, a dropped connection) reads as a session, which is
// what a root has always been: the session view then reports the network state
// through its composer status rather than a second failure screen.

import { useCallback, useEffect, useState } from "react";
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
  /** The host's root, with no credential yet — the listing is guarded. */
  | { kind: "unauthenticated" };

export async function resolveEntry(): Promise<Entry> {
  if (apiBase() !== "") return { kind: "session" };
  try {
    return { kind: "list", sessions: await fetchSessions() };
  } catch (error) {
    if (error instanceof ApiError && error.status === 401) {
      void bearer.clear();
      return { kind: "unauthenticated" };
    }
    return { kind: "session" };
  }
}

/** The binding, once known; `null` while the probe is out. `resolve` re-runs it
 * — what the login view calls once a credential exists, and what a list reload
 * is.
 *
 * `base` is the page's current base, which navigation changes without a load
 * (ui/route.ts): under a prefix the binding is a session's view and is known at
 * once; at the root the probe's last answer stays up while the next one is out,
 * so returning to the list from a session paints the list it left. */
export function useEntry(base: string): {
  entry: Entry | null;
  resolve: () => void;
} {
  const [root, setRoot] = useState<Entry | null>(null);
  const resolve = useCallback(() => {
    void resolveEntry().then(setRoot);
  }, []);
  useEffect(() => {
    if (base === "") resolve();
  }, [base, resolve]);
  // Before any probe has answered, a listing some session's switcher heard is
  // as good a seed for the list as the probe's own.
  const heard = lastListing();
  const seeded: Entry | null =
    heard === null ? null : { kind: "list", sessions: heard };
  return {
    entry: base === "" ? (root ?? seeded) : { kind: "session" },
    resolve,
  };
}
