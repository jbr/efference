// The stores of recently visited sessions (docs/web-client.md): leaving a
// session keeps its `Store` — transcript window, topics, cursor — so coming
// back renders the last snapshot at once and the connection resumes from the
// cursor instead of starting from an empty page.
//
// Warm means a retained snapshot, not an open connection: a stream open for a
// session nobody is looking at would count as someone watching and suppress
// that session's push notifications (docs/web-push.md). The connector is the
// view's (session/useSession.ts) and closes when the view does.

import { fetchLogTail } from "../api/http.ts";
import { CATCH_UP_TAIL } from "./connect.ts";
import { reseed, Store } from "./store.ts";

/** How many sessions keep a snapshot. The current session is one of them. */
export const WARM_SESSIONS = 6;

// Map order is recency: the most recently acquired is last.
const stores = new Map<string, Store>();

// Sessions read ahead of a visit (`prefetchStore`) but not yet visited. Kept
// apart so that glancing at tabs cannot push a session already viewed out of
// `stores`; a visit promotes the entry.
const prefetched = new Map<string, Store>();
const PREFETCHED = 3;

/** The store for the session at `base` (`""` at a session daemon's root),
 * created empty on the first visit. Acquiring makes it the most recent; the
 * least recent beyond `WARM_SESSIONS` are dropped. */
export function acquireStore(base: string): Store {
  let store = stores.get(base) ?? prefetched.get(base);
  prefetched.delete(base);
  if (store === undefined) store = new Store();
  else stores.delete(base);
  stores.set(base, store);
  for (const stale of [...stores.keys()].slice(
    0,
    Math.max(0, stores.size - WARM_SESSIONS),
  )) {
    stores.delete(stale);
  }
  return store;
}

/**
 * Read a session's log tail ahead of a visit, so that entering it paints a
 * transcript instead of an empty page: one `GET /log?tail=`, no stream, so the
 * session is not counted as watched (docs/web-push.md). Does nothing for a
 * session already held. The read is applied only to a store nobody has touched
 * since — a visit that began while it was out has its own catch-up, whose
 * cursor a late tail must not move backwards.
 */
export function prefetchStore(base: string): void {
  if (stores.has(base) || prefetched.has(base)) return;
  const store = new Store();
  prefetched.set(base, store);
  for (const stale of [...prefetched.keys()].slice(
    0,
    Math.max(0, prefetched.size - PREFETCHED),
  )) {
    prefetched.delete(stale);
  }
  fetchLogTail(CATCH_UP_TAIL, base).then(
    (entries) => {
      if (store.snapshot().lastSeq === null) {
        store.update((s) => reseed(s, entries));
      }
    },
    // A failed read-ahead is a cold visit, which is what it would have been.
    () => prefetched.delete(base),
  );
}

/** Drop a session's snapshot — one the host no longer has, or a test's. */
export function forgetStore(base: string): void {
  stores.delete(base);
  prefetched.delete(base);
}
