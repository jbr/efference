// The lightbox's dismiss-swipe rule (swipe.ts): a deliberate drag or a quick
// flick dismisses, and a tap — or the jitter that passes for one — does not.

import { expect, test } from "vitest";
import { dismisses } from "./swipe.ts";

test("a deliberate drag dismisses whatever its speed", () => {
  expect(dismisses({ dx: 140, dy: 0, elapsed: 800 })).toBe(true);
  expect(dismisses({ dx: 0, dy: -130, elapsed: 500 })).toBe(true);
});

test("a quick flick dismisses; the same distance dragged slowly does not", () => {
  expect(dismisses({ dx: 40, dy: 0, elapsed: 40 })).toBe(true);
  expect(dismisses({ dx: 40, dy: 0, elapsed: 400 })).toBe(false);
});

test("a tap, or a drag too small to mean anything, springs back", () => {
  expect(dismisses({ dx: 0, dy: 0, elapsed: 50 })).toBe(false);
  expect(dismisses({ dx: 8, dy: -6, elapsed: 30 })).toBe(false);
});
