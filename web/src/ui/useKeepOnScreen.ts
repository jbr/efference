import { useLayoutEffect, useRef } from "react";

/** Keep an anchored popup between the viewport's edges (docs/web-client.md). */
export function useKeepOnScreen(open = true) {
  const ref = useRef<HTMLDivElement>(null);
  useLayoutEffect(() => {
    if (!open) return;
    const fit = () => {
      const element = ref.current;
      if (element === null) return;
      element.style.transform = "";
      const bounds = element.getBoundingClientRect();
      const offset =
        bounds.left < 8
          ? 8 - bounds.left
          : Math.min(0, window.innerWidth - 8 - bounds.right);
      element.style.transform = `translateX(${offset}px)`;
    };
    fit();
    window.addEventListener("resize", fit);
    return () => window.removeEventListener("resize", fit);
  });
  return ref;
}
