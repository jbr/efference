// History-backed overlays (docs/web-client.md "The phone breakpoint"): each
// open overlay (inspector sheet, file viewer) owns one history entry, so the
// back button — and Safari's edge swipe, which *is* history-back — dismisses
// the overlay instead of leaving the app. This is not a router: the overlays
// remain App state; history only mirrors how many are open.
//
// The sync is reconciliation, not instrumentation: App reports how many
// overlays are open, the hook pushes or consumes entries until the pushed
// count matches. That way every existing close path (✕, Escape, tapping the
// inspected call again) converges without touching history itself — the
// reconciler notices the count dropped and consumes the stale entry with
// history.back(), whose popstate the handler recognizes as already settled.
//
// Sessions share one page (ui/route.ts), so an overlay entry can outlive the
// view that pushed it: switching sessions with an overlay open leaves its entry
// behind. A pop onto it is the new view's to settle — the view that pushed it is
// already on its way out, and its handler stands down once the page has moved to
// another session — and a view that mounts on such an entry consumes it, since
// nothing says which overlay it stood for. The exception is a view that mounts
// with an overlay already open (`/tree`, reloaded): the entry it finds is the one
// that overlay would push, so it is adopted rather than consumed.

import { useEffect, useRef } from "react";
import { apiBase } from "../api/base.ts";

export function useOverlayHistory(openCount: number, closeTop: () => void) {
  const pushed = useRef(0);
  const owner = useRef(apiBase());
  // The entry the page held when this view first rendered, read before any of
  // the effects below can push one: StrictMode re-runs the mount effect after
  // the push effect's first run, and reading `history.state` there would find
  // this view's own entry and consume it.
  const mountedOn = useRef(history.state as { efOverlay?: boolean } | null);
  const consumedStale = useRef(false);
  const count = useRef(openCount);
  const close = useRef(closeTop);
  useEffect(() => {
    count.current = openCount;
    close.current = closeTop;
  });

  useEffect(() => {
    // Mounting onto an entry some earlier view pushed. `consumedStale` keeps
    // StrictMode's second effect run from stepping back twice.
    if (mountedOn.current?.efOverlay === true && !consumedStale.current) {
      consumedStale.current = true;
      if (count.current > 0) pushed.current = 1;
      else history.back();
    }
    const onPop = (event: PopStateEvent) => {
      if (apiBase() !== owner.current) return;
      const state = event.state as { efOverlay?: boolean } | null;
      if (state?.efOverlay === true) {
        // Forward navigation onto an overlay entry. Which overlay it stood
        // for is not recorded, so nothing can reopen — consume it again.
        pushed.current += 1;
        if (pushed.current > count.current) history.back();
      } else {
        pushed.current = Math.max(0, pushed.current - 1);
        if (count.current > pushed.current) close.current();
      }
    };
    window.addEventListener("popstate", onPop);
    return () => window.removeEventListener("popstate", onPop);
  }, []);

  useEffect(() => {
    if (openCount > pushed.current) {
      history.pushState({ efOverlay: true }, "");
      pushed.current += 1;
    } else if (openCount < pushed.current) {
      // Closed from the UI: consume the entry the overlay owned. The
      // resulting popstate finds the counts already matched and closes
      // nothing.
      history.back();
    }
  }, [openCount]);
}
