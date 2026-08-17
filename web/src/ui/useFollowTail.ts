// Follow-tail autoscroll (docs/web-client.md): the transcript scrolls the
// window, so "following" means the viewport sits at the document's bottom.
// Follow while there; hold position once the reader scrolls up. Runs after
// every render rather than diffing inputs — a render that didn't grow the
// document makes the scroll a no-op, and one that did is exactly when
// following should land the tail. Starts true, so a cold open shows the tail
// rather than the top of the preamble.

import { useLayoutEffect, useRef } from "react";

/** How close to the bottom still counts as "at" it, in px — a partial line
 * of streamed output must not silently disengage the follow. */
const SLACK = 48;

export function useFollowTail() {
  const following = useRef(true);
  useLayoutEffect(() => {
    const update = () => {
      following.current =
        window.innerHeight + window.scrollY >=
        document.documentElement.scrollHeight - SLACK;
    };
    window.addEventListener("scroll", update, { passive: true });
    window.addEventListener("resize", update, { passive: true });
    return () => {
      window.removeEventListener("scroll", update);
      window.removeEventListener("resize", update);
    };
  }, []);
  useLayoutEffect(() => {
    if (following.current) {
      window.scrollTo(0, document.documentElement.scrollHeight);
    }
  });
}
