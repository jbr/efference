// The lightbox's zoom arithmetic (docs/web-client.md): a scale about the
// image's center plus a translation, in stage-client pixels. The rule that
// makes zooming feel right is that the image point under the fingers (or the
// cursor) stays under them; it lives here rather than in the component so it
// can be checked without a DOM.

export interface Point {
  x: number;
  y: number;
}

export interface Transform {
  scale: number;
  x: number;
  y: number;
}

export interface Size {
  width: number;
  height: number;
}

export const IDENTITY: Transform = { scale: 1, x: 0, y: 0 };

/** Fit is 1; the ceiling is well past the point a screenshot's pixels stop
 * resolving on a phone. */
export const MIN_SCALE = 1;
export const MAX_SCALE = 8;

/** What a double-tap (or double-click) zooms to, and back from. */
export const TAP_SCALE = 2.5;

const clamp = (value: number, lo: number, hi: number) =>
  Math.min(hi, Math.max(lo, value));

export const clampScale = (scale: number) => clamp(scale, MIN_SCALE, MAX_SCALE);

/** Scale `start` toward `scale` while the image point under `from` moves to
 * `to`, both in client coordinates with the stage's `center`. Equal `from` and
 * `to` is a zoom about a fixed point (wheel, double-tap); differing ones are
 * the two-finger pinch, whose midpoint drifts as the fingers move. */
export function zoomAbout(
  start: Transform,
  scale: number,
  from: Point,
  to: Point,
  center: Point,
): Transform {
  const next = clampScale(scale);
  const ratio = next / start.scale;
  return {
    scale: next,
    x: to.x - center.x - (from.x - center.x - start.x) * ratio,
    y: to.y - center.y - (from.y - center.y - start.y) * ratio,
  };
}

/** One-finger drag by `(dx, dy)`, then held inside the stage. */
export function panBy(
  start: Transform,
  dx: number,
  dy: number,
  stage: Size,
  base: Size,
): Transform {
  return clampTranslate(
    { scale: start.scale, x: start.x + dx, y: start.y + dy },
    stage,
    base,
  );
}

/** Never leave a gap inside the stage: at fit the image is pinned to center,
 * and once it is larger than the stage each edge stops at the stage edge.
 * `base` is the image's on-screen size at scale 1 and `stage` the stage's. */
export function clampTranslate(
  transform: Transform,
  stage: Size,
  base: Size,
): Transform {
  const maxX = Math.max(0, (base.width * transform.scale - stage.width) / 2);
  const maxY = Math.max(0, (base.height * transform.scale - stage.height) / 2);
  return {
    scale: transform.scale,
    x: clamp(transform.x, -maxX, maxX),
    y: clamp(transform.y, -maxY, maxY),
  };
}

/** The double-tap toggle: back to fit when already zoomed, otherwise
 * `TAP_SCALE` about the tapped point. */
export function toggleZoom(
  current: Transform,
  at: Point,
  center: Point,
): Transform {
  if (current.scale > MIN_SCALE) return IDENTITY;
  return zoomAbout(current, TAP_SCALE, at, at, center);
}

/** Distance and midpoint of two fingers, for the pinch. */
export function fingerSpan(a: Point, b: Point): { dist: number; mid: Point } {
  const dx = a.x - b.x;
  const dy = a.y - b.y;
  return {
    dist: Math.hypot(dx, dy),
    mid: { x: (a.x + b.x) / 2, y: (a.y + b.y) / 2 },
  };
}
