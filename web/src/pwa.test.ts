import { expect, it } from "vitest";
import { workerUrl } from "./pwa.ts";

it("names the non-caching worker for the dev path only", () => {
  expect(workerUrl(true)).toBe("/sw.js");
  expect(workerUrl(false)).toBe("/sw.js?dev=1");
});
