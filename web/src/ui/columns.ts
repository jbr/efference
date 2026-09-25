// How many monospace columns fit in an element (docs/web-client.md "A call
// reads as a signature"): the one measurement `signatureLayout.ts` needs.
//
// One shared ResizeObserver watches every element that asked, and each hook
// re-renders only when its *integer* column count changes — so dragging a
// window re-lays out a signature a handful of times, not once per pixel, and
// a transcript of several hundred calls costs one observer. The character
// width is measured once per font (a canvas `measureText`, no layout) and
// forgotten when a web font finishes loading, since the fallback face that
// painted first has a different advance.

import { useLayoutEffect, useState } from "react";

const advances = new Map<string, number>();
let context: CanvasRenderingContext2D | null = null;

/** The advance of one character in the element's font, in CSS pixels. */
function advance(element: Element): number {
  const style = getComputedStyle(element);
  const font = `${style.fontStyle} ${style.fontWeight} ${style.fontSize} ${style.fontFamily}`;
  const known = advances.get(font);
  if (known !== undefined) return known;
  context ??= document.createElement("canvas").getContext("2d");
  if (context === null) return 0;
  context.font = font;
  // Ten characters, so a sub-pixel advance is not rounded away.
  const measured = context.measureText("0000000000").width / 10;
  advances.set(font, measured);
  return measured;
}

/** The whole columns in the element's content box, or null when it has no
 * width to measure (not laid out, or no canvas). */
function columnsOf(element: Element, contentWidth: number): number | null {
  const each = advance(element);
  return each > 0 && contentWidth > 0 ? Math.floor(contentWidth / each) : null;
}

const listeners = new Map<Element, (columns: number | null) => void>();
let observer: ResizeObserver | null = null;

function observe(
  element: Element,
  listener: (columns: number | null) => void,
): () => void {
  observer ??= new ResizeObserver((entries) => {
    for (const entry of entries) {
      listeners.get(entry.target)?.(
        columnsOf(entry.target, entry.contentRect.width),
      );
    }
  });
  listeners.set(element, listener);
  observer.observe(element);
  return () => {
    listeners.delete(element);
    observer?.unobserve(element);
  };
}

// A web font arriving changes the advance without resizing anything, so the
// observer would never say so: re-measure every watched element instead.
if (typeof document !== "undefined" && "fonts" in document) {
  document.fonts.addEventListener("loadingdone", () => {
    advances.clear();
    for (const [element, listener] of listeners) {
      listener(columnsOf(element, element.getBoundingClientRect().width));
    }
  });
}

/**
 * The column count of the element the returned ref is attached to — null
 * until it is measured. The first measurement happens before paint, so a
 * signature's first frame is already laid out at its width.
 */
export function useColumns(): [
  (element: HTMLElement | null) => void,
  number | null,
] {
  const [element, setElement] = useState<HTMLElement | null>(null);
  const [columns, setColumns] = useState<number | null>(null);
  useLayoutEffect(() => {
    if (element === null) return;
    const style = getComputedStyle(element);
    const padding =
      parseFloat(style.paddingLeft) + parseFloat(style.paddingRight);
    // Layout effects run before paint, so this frame is laid out once the
    // first measurement lands; the observer takes it from there.
    // eslint-disable-next-line react-hooks/set-state-in-effect -- the first measurement must land before paint; the observer's first callback arrives a frame late.
    setColumns(columnsOf(element, element.clientWidth - padding));
    return observe(element, setColumns);
  }, [element]);
  return [setElement, columns];
}
