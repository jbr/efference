// Whether an element has come near the viewport (docs/web-client.md): the gate
// for work that should wait until its result could be seen, such as an image
// request.

import { useEffect, useState, type RefObject } from "react";

/** How far past the viewport's top and bottom edges counts as near, about the
 * distance at which browsers start native lazy images. The transcript scrolls
 * vertically, so the sides get none. */
const NEAR = "1500px 0px";

/**
 * Whether `target` has been within `margin` of the viewport since the component
 * mounted. It turns true once and stays true, so scrolling away does not undo
 * what the arrival started. Without `IntersectionObserver` there is no distance
 * to measure, and it is true from the first render.
 */
export function useNearViewport(
  target: RefObject<Element | null>,
  margin: string = NEAR,
): boolean {
  const supported = typeof IntersectionObserver !== "undefined";
  const [near, setNear] = useState(false);
  useEffect(() => {
    const element = target.current;
    if (!supported || near || element === null) return;
    const observer = new IntersectionObserver(
      (entries) => {
        if (entries.some((entry) => entry.isIntersecting)) setNear(true);
      },
      { rootMargin: margin },
    );
    observer.observe(element);
    return () => observer.disconnect();
  }, [target, margin, supported, near]);
  return near || !supported;
}
