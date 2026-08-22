// Shared machinery for surfaces floating above the dock (docs/web-client.md):
// the gate float/cue and the jump-to-latest chip. Extracted from Gate.tsx when
// the second consumer arrived.

import { useEffect, useState } from "react";

/** The dock's measured height plus a margin: the floating surfaces sit just
 * above the dock, whose height moves (the bottom panel expands and
 * collapses, the textarea resizes) — so the offset is measured, not guessed
 * in CSS. */
export function useDockOffset(): number {
  const [bottom, setBottom] = useState(120);
  useEffect(() => {
    const dock = document.querySelector(".dock");
    if (!(dock instanceof HTMLElement)) return;
    const update = () => setBottom(dock.offsetHeight + 8);
    update();
    const observer = new ResizeObserver(update);
    observer.observe(dock);
    return () => observer.disconnect();
  }, []);
  return bottom;
}

/** The jump the floating surfaces offer. It scrolls to the document tail
 * rather than to the surface: a gated call has no result entry, so the gate
 * *is* the tail, and going there brings the call and its verdict along. It
 * also has to be a jump, not a smooth `scrollIntoView`. `useFollowTail` pins
 * to the bottom after every render while the viewport is near it, so an
 * animated scroll toward the tail re-engages the follow partway down and is
 * cancelled by the next render — including the one caused by the in-flow
 * surface coming into view. */
export const jumpToTail = () =>
  window.scrollTo(0, document.documentElement.scrollHeight);
