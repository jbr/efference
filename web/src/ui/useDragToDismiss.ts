import { useRef, type PointerEvent } from "react";

/** The velocity projection horizon: how far ahead the finger's momentum would
 * carry the sheet, deciding whether a flick dismisses even if the drag stopped
 * short of the midpoint. */
const PROJECTION_MS = 180;

/** Only the last stretch of a drag is its release velocity: a drag that sped
 * up and then slowed to a stop was not a flick. */
const VELOCITY_WINDOW_MS = 80;

/** Drag a bottom sheet / drawer down by its handle to dismiss it.
 *
 * The element follows the finger; on release it closes if where its
 * momentum would carry it is past half its height, and otherwise springs
 * back. The transform is set on the element directly so a drag re-renders
 * nothing. */
export function useDragToDismiss<T extends HTMLElement = HTMLElement>(
  onClose: () => void,
) {
  const sheet = useRef<T>(null);
  const drag = useRef<{ y: number; samples: [number, number][] } | null>(null);
  const offset = (event: PointerEvent) =>
    Math.max(0, event.clientY - (drag.current?.y ?? event.clientY));

  const onPointerDown = (event: PointerEvent) => {
    if ((event.target as HTMLElement).closest("button, textarea, input"))
      return;
    drag.current = { y: event.clientY, samples: [[event.timeStamp, 0]] };
    (event.currentTarget as HTMLElement).setPointerCapture(event.pointerId);
    if (sheet.current) sheet.current.style.transition = "none";
  };

  const onPointerMove = (event: PointerEvent) => {
    if (!drag.current || !sheet.current) return;
    const dy = offset(event);
    const samples = drag.current.samples;
    samples.push([event.timeStamp, dy]);
    // Above its resting place the sheet stretches with resistance rather
    // than stopping dead, the way an iOS sheet does when pushed back up.
    const raw = event.clientY - drag.current.y;
    const shown = raw < 0 ? -Math.sqrt(-raw) * 2 : dy;
    while (
      samples.length > 2 &&
      event.timeStamp - samples[0][0] > VELOCITY_WINDOW_MS
    ) {
      samples.shift();
    }
    sheet.current.style.transform = `translateY(${shown}px)`;
  };

  const onPointerUp = (event: PointerEvent) => {
    const state = drag.current;
    const element = sheet.current;
    const dy = offset(event);
    drag.current = null;
    if (!state || !element) return;
    const [t0, y0] = state.samples[0];
    const velocity = (dy - y0) / Math.max(1, event.timeStamp - t0);
    element.style.transition = "";
    element.style.transform = "";
    if (dy + velocity * PROJECTION_MS > element.offsetHeight / 2) onClose();
  };

  return {
    sheet,
    handlers: {
      onPointerDown,
      onPointerMove,
      onPointerUp,
      onPointerCancel: onPointerUp,
    },
  };
}
