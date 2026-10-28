import { expect, test } from "vitest";
import { newerVersionLine } from "./newerVersion.ts";

test("a known newer release is stated by its version", () => {
  expect(
    newerVersionLine({
      version: "0.2.0",
      tag: "v0.2.0",
      url: "https://efference.net/releases/v0.2.0",
      update_command: "curl -fsSL https://efference.net/install.sh | sh",
    }),
  ).toBe("efference 0.2.0 is available");
});

test("no newer release is no line", () => {
  expect(newerVersionLine(null)).toBeNull();
});
