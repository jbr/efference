import { expect, test } from "vitest";
import { isHandoff } from "./handoff.ts";
test("explicit and automatic handoffs require matching structured handles", () => {
  expect(isHandoff("backgrounded as p#3. instructions", "p#3")).toBe(true);
  expect(
    isHandoff(
      "this command was still running after 30s, so it moved to the background on its own as p#3. nothing was killed and no output was lost.",
      "p#3",
    ),
  ).toBe(true);
  expect(isHandoff("backgrounded as p#3. instructions", undefined)).toBe(false);
  expect(isHandoff("backgrounded as p#3. instructions", "p#4")).toBe(false);
  expect(isHandoff("real stdout with p#3", "p#3")).toBe(false);
});
