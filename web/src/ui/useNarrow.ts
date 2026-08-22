// The one JS-side reading of the phone breakpoint (700px, paired with the
// `@media (max-width: 700px)` block in index.css — change both together).
// Display components use it for what CSS alone can't switch: truncation
// depth, placeholder copy. Interim usability pass; the real mobile design
// pass may replace it (backlog/web/README.md).

import { useSyncExternalStore } from "react";

const query = window.matchMedia("(max-width: 700px)");

function subscribe(onChange: () => void): () => void {
  query.addEventListener("change", onChange);
  return () => query.removeEventListener("change", onChange);
}

export function useNarrow(): boolean {
  return useSyncExternalStore(subscribe, () => query.matches);
}
