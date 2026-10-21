import { expect, it } from "vitest";
import { workerUrl } from "./pwa.ts";
import worker from "../public/sw.js?raw";

it("names the non-caching worker for the dev path only", () => {
  expect(workerUrl(true)).toBe("/sw.js");
  expect(workerUrl(false)).toBe("/sw.js?dev=1");
});

it("clamps notification-click targets to the worker's origin", () => {
  // The worker is served raw from public/, unbundled, so nothing imports it —
  // the pin is textual for the same reason the wire-bytes pins are: it fails
  // when the clamp is deleted, and is updated deliberately when it moves. The
  // push payload's `path` is the sender's choice; without the clamp an
  // absolute URL would carry `openWindow` off-origin.
  expect(worker).toContain("if (target.origin !== self.location.origin)");
});
