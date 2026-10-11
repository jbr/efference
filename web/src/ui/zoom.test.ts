// The lightbox's zoom arithmetic (zoom.ts), asserted directly: the property
// that matters is anchoring — the image point under the fingers or the cursor
// does not move when the scale changes.

import { expect, test } from "vitest";
import {
  IDENTITY,
  MAX_SCALE,
  MIN_SCALE,
  TAP_SCALE,
  clampScale,
  clampTranslate,
  fingerSpan,
  panBy,
  toggleZoom,
  zoomAbout,
  type Transform,
} from "./zoom.ts";

const CENTER = { x: 0, y: 0 };

test("scale is clamped to the fit-to-ceiling range", () => {
  expect(clampScale(0.2)).toBe(MIN_SCALE);
  expect(clampScale(3)).toBe(3);
  expect(clampScale(99)).toBe(MAX_SCALE);
});

test("a zoom about a fixed point holds that point still", () => {
  const at = { x: 100, y: 50 };
  const zoomed = zoomAbout(IDENTITY, 2, at, at, CENTER);
  // Where the anchored image point lands on screen again.
  const screen = {
    x: CENTER.x + zoomed.x + at.x * zoomed.scale,
    y: CENTER.y + zoomed.y + at.y * zoomed.scale,
  };
  expect(zoomed.scale).toBe(2);
  expect(screen).toEqual(at);
});

test("a pinch holds the image point under the initial midpoint", () => {
  const from = { x: 100, y: 100 };
  const to = { x: 120, y: 100 };
  const pinched = zoomAbout(IDENTITY, 2, from, to, CENTER);
  const screen = {
    x: CENTER.x + pinched.x + from.x * pinched.scale,
    y: CENTER.y + pinched.y + from.y * pinched.scale,
  };
  expect(screen).toEqual(to);
});

test("at fit the image is pinned to center; zoomed it stops at the edges", () => {
  const stage = { width: 100, height: 100 };
  const base = { width: 100, height: 100 };
  const dragged: Transform = { scale: 1, x: 40, y: -40 };
  const pinned = clampTranslate(dragged, stage, base);
  // A limit of zero can come back as -0, which is not meaningful here.
  expect({ ...pinned, x: pinned.x || 0, y: pinned.y || 0 }).toEqual(IDENTITY);
  // At 2x the overflow is 100px each way, so half of it is the limit.
  const zoomed: Transform = { scale: 2, x: 999, y: -999 };
  expect(clampTranslate(zoomed, stage, base)).toEqual({
    scale: 2,
    x: 50,
    y: -50,
  });
});

test("a pan is a translate, held inside the stage", () => {
  const stage = { width: 200, height: 100 };
  const base = { width: 200, height: 100 };
  const zoomed: Transform = { scale: 2, x: 0, y: 0 };
  // 400×200 at 2x in a 200×100 stage: 100px of slack on x, 50px on y.
  expect(panBy(zoomed, 30, 999, stage, base)).toEqual({
    scale: 2,
    x: 30,
    y: 50,
  });
});

test("double-tap toggles between fit and the tap scale, about the tapped point", () => {
  const at = { x: 20, y: 10 };
  const zoomed = toggleZoom(IDENTITY, at, CENTER);
  expect(zoomed.scale).toBe(TAP_SCALE);
  expect(toggleZoom(zoomed, at, CENTER)).toEqual(IDENTITY);
});

test("fingerSpan gives the pinching fingers' distance and midpoint", () => {
  expect(fingerSpan({ x: 0, y: 0 }, { x: 3, y: 4 })).toEqual({
    dist: 5,
    mid: { x: 1.5, y: 2 },
  });
});
