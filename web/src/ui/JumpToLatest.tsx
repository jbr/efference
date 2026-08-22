// The jump-to-latest chip: a floating affordance above the dock, shown only
// while the reader has scrolled away from the tail (docs/web-client.md —
// follow-tail holds position once you scroll up, and this is the way back).
// It never scrolls the page on its own; the tap is an instant jump, which
// lands inside `useFollowTail`'s slack and so re-engages following.

import { useEffect, useState } from "react";
import { jumpToTail, useDockOffset } from "./float.ts";

/** Matches useFollowTail's SLACK so the chip and the follow agree about
 * what counts as "at" the tail. */
const SLACK = 48;

/** Whether the viewport sits at the document tail — the render-triggering
 * reading. `useFollowTail` keeps its own in a ref on purpose (it must not
 * re-render per scroll); this one flips a boolean, so it re-renders exactly
 * twice per depart-and-return. */
function useAtTail(): boolean {
  const [atTail, setAtTail] = useState(true);
  useEffect(() => {
    const update = () =>
      setAtTail(
        window.innerHeight + window.scrollY >=
          document.documentElement.scrollHeight - SLACK,
      );
    update();
    window.addEventListener("scroll", update, { passive: true });
    window.addEventListener("resize", update, { passive: true });
    const observer = new ResizeObserver(update);
    observer.observe(document.documentElement);
    return () => {
      window.removeEventListener("scroll", update);
      window.removeEventListener("resize", update);
      observer.disconnect();
    };
  }, []);
  return atTail;
}

export function JumpToLatest({ hidden }: { hidden: boolean }) {
  const atTail = useAtTail();
  const bottom = useDockOffset();
  if (hidden || atTail) return null;
  return (
    <button className="jump-latest" style={{ bottom }} onClick={jumpToTail}>
      ↓ latest
    </button>
  );
}
