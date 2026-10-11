// The lightbox's swipe-to-dismiss rule (docs/web-client.md): a one-finger drag
// at fit is a dismissal gesture, in any direction, rather than a pan — there is
// nothing to pan until the image is zoomed. Kept apart from the zoom arithmetic
// so the rule is testable without a DOM.

export interface Swipe {
  /** Where the finger ended, relative to where it started. */
  dx: number;
  dy: number;
  /** How long the gesture took, in ms. */
  elapsed: number;
}

/** A deliberate drag this far dismisses, whatever its speed. */
const DISMISS_PX = 120;

/** ...or a quick flick, provided it still travelled this far — so the small
 * jitter of a tap never dismisses. */
const FLICK_PX = 30;

/** The speed (px/ms) that makes a short drag a flick. */
const FLICK_SPEED = 0.5;

/** Whether a released swipe dismisses: far enough on its own, or a quick flick
 * that got far enough to be intentional. */
export function dismisses(swipe: Swipe): boolean {
  const distance = Math.hypot(swipe.dx, swipe.dy);
  if (distance >= DISMISS_PX) return true;
  return (
    distance >= FLICK_PX && distance / Math.max(1, swipe.elapsed) >= FLICK_SPEED
  );
}
