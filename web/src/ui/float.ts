// Shared machinery for surfaces floating above the dock (docs/web-client.md):
// the gate float/cue and the jump-to-latest chip. Extracted from Gate.tsx when
// the second consumer arrived.

import { useEffect, useState } from "react";
import { resumeTail } from "./followTail.ts";

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

/** Gate/latest jumps explicitly resume window-tail following (docs/web-client.md). */
export const jumpToTail = resumeTail;
