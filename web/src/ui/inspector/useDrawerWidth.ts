import { useRef, useState } from "react";

/** Shared persisted width for operation and current-file inspection. CSS
 * constrains the sibling column on desktop and the overlay on narrow screens
 * (docs/web-client.md). */
const WIDTH_KEY = "inspector-width";
const clampWidth = (w: number) =>
  Math.min(Math.max(w, 280), Math.max(320, window.innerWidth - 320));

export function useDrawerWidth() {
  const [width, setWidth] = useState(() =>
    clampWidth(Number(localStorage.getItem(WIDTH_KEY)) || 396),
  );
  const dragging = useRef(false);
  const grip = {
    onPointerDown: (e: React.PointerEvent<HTMLDivElement>) => {
      dragging.current = true;
      e.currentTarget.setPointerCapture(e.pointerId);
      e.preventDefault();
    },
    onPointerMove: (e: React.PointerEvent<HTMLDivElement>) => {
      if (!dragging.current) return;
      const w = clampWidth(window.innerWidth - e.clientX);
      setWidth(w);
      localStorage.setItem(WIDTH_KEY, String(w));
    },
    onPointerUp: (e: React.PointerEvent<HTMLDivElement>) => {
      dragging.current = false;
      e.currentTarget.releasePointerCapture(e.pointerId);
    },
  };
  return { width, grip };
}
