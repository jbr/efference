import { describe, expect, it } from "vitest";
import {
  INLINE_LINE_LIMIT,
  describeKind,
  pastedName,
  pastesAsFile,
} from "./staging.ts";

describe("pastesAsFile", () => {
  // Counted as the TUI counts (`str::lines`), so a paste lands the same way in
  // either client: a trailing newline ends the last line rather than adding one.
  it("keeps up to the limit inline and stages past it", () => {
    const lines = (n: number) =>
      Array.from({ length: n }, () => "x").join("\n");
    expect(pastesAsFile(lines(INLINE_LINE_LIMIT))).toBe(false);
    expect(pastesAsFile(`${lines(INLINE_LINE_LIMIT)}\n`)).toBe(false);
    expect(pastesAsFile(lines(INLINE_LINE_LIMIT + 1))).toBe(true);
    expect(pastesAsFile("")).toBe(false);
  });
});

describe("describeKind", () => {
  it("states the signals the model's notice states, short", () => {
    expect(
      describeKind({
        Image: { format: "PNG", width: 1280, height: 800, bytes: 245_760 },
      }),
    ).toBe("PNG · 1280×800 · 240.0 KB");
    expect(
      describeKind({
        Text: { bytes: 2048, lines: 40, shape: "Json", first_line: "{" },
      }),
    ).toBe("40 lines · 2.0 KB");
    expect(describeKind({ Binary: { bytes: 12 } })).toBe("12 B");
  });
});

describe("pastedName", () => {
  // A clipboard image arrives as `image.png` every time; a dropped file keeps
  // its own name.
  it("names a pasted image for when it arrived", () => {
    const at = new Date("2026-09-27T21:07:55Z");
    const pasted = new File([""], "image.png", { type: "image/png" });
    expect(pastedName(pasted, at)).toBe("paste-2026-09-27-21-07-55.png");
    const dropped = new File([""], "diagram.svg", { type: "image/svg+xml" });
    expect(pastedName(dropped, at)).toBe("diagram.svg");
  });
});
