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

import { useEffect, useRef } from "react";

export function useOverlayHistory(openCount: number, closeTop: () => void) {
  const pushed = useRef(0);
  const count = useRef(openCount);
  const close = useRef(closeTop);
  useEffect(() => {
    count.current = openCount;
    close.current = closeTop;
  });

  useEffect(() => {
    const onPop = (event: PopStateEvent) => {
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
