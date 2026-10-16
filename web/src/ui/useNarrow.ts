// The one JS-side reading of the phone breakpoint (700px, paired with the
// `@media (max-width: 700px)` block in index.css — change both together).
// Display components use it for what CSS alone can't switch: truncation
// depth, placeholder copy. Interim usability pass; the real mobile design
// pass may replace it (backlog/inbox/web-mobile-inspection-design.md).

import { useSyncExternalStore } from "react";

// Read on first use rather than at module scope: a module that touches
// `window` while loading cannot be imported by a DOM-free test, and the
// components that read this are rendered to static markup in one.
let media: MediaQueryList | null = null;
const query = (): MediaQueryList | null => {
  if (
    typeof window === "undefined" ||
    typeof window.matchMedia !== "function"
  ) {
    return null;
  }
  return (media ??= window.matchMedia("(max-width: 700px)"));
};

function subscribe(onChange: () => void): () => void {
  const list = query();
  if (!list) return () => {};
  list.addEventListener("change", onChange);
  return () => list.removeEventListener("change", onChange);
}

export function useNarrow(): boolean {
  return useSyncExternalStore(
    subscribe,
    () => query()?.matches ?? false,
    () => false,
  );
}
