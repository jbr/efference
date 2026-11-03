// The notice that this page is running an older build than the host serves
// (api/bundle.ts, docs/web-client.md "A page outlives a host upgrade"). It states
// the fact and offers the one action that settles it — a button, because an
// installed app has no browser chrome to reload with.
//
// Not the session's banner: that one carries loop state above the transcript.
// This is app chrome, and it lives in the shared header (ui/AppHeader.tsx), so
// every view of the app shows it.

import { useSyncExternalStore } from "react";
import { bundleState, subscribeBundle } from "../api/bundle.ts";

/** The strip itself, with no store behind it — the gallery's specimen. */
export function BundleNoticeStrip({ onReload }: { onReload: () => void }) {
  return (
    <div
      className="bundle-notice"
      role="status"
      title="This page is running the build it was loaded with; the host serves a newer one."
    >
      <span>efference was updated</span>
      <button type="button" className="bundle-reload" onClick={onReload}>
        Reload
      </button>
    </div>
  );
}

export function BundleNotice() {
  const stale = useSyncExternalStore(subscribeBundle, bundleState, bundleState);
  if (stale !== "stale") return null;
  return <BundleNoticeStrip onReload={() => location.reload()} />;
}
