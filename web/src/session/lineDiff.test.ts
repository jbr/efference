import { describe, expect, it } from "vitest";
import { diffLines } from "./lineDiff.ts";

const join = (...lines: string[]) => lines.join("\n");

describe("diffLines", () => {
  it("identical texts report no changes", () => {
    const text = join("a", "b", "c");
    expect(diffLines(text, text)).toEqual({ changed: [], removedAt: [] });
  });

  it("a modified line is a one-line changed range", () => {
    expect(diffLines(join("a", "b", "c"), join("a", "B", "c"))).toEqual({
      changed: [[2, 2]],
      removedAt: [],
    });
  });

  it("inserted lines are a changed range at their new position", () => {
    expect(diffLines(join("a", "d"), join("a", "b", "c", "d"))).toEqual({
      changed: [[2, 3]],
      removedAt: [],
    });
  });

  it("a pure deletion is a deletion point, not a range", () => {
    expect(diffLines(join("a", "b", "c", "d"), join("a", "d"))).toEqual({
      changed: [],
      removedAt: [1],
    });
  });

  it("deletion before the first line points at 0", () => {
    expect(diffLines(join("x", "a", "b"), join("a", "b"))).toEqual({
      changed: [],
      removedAt: [0],
    });
  });

  it("a replacement counts as changed, not as a deletion point", () => {
    // Two old lines become one new line: the run has an insertion, so the
    // deletions fold into "modified" rather than reporting a deletion point too.
    expect(diffLines(join("a", "x", "y", "d"), join("a", "z", "d"))).toEqual({
      changed: [[2, 2]],
      removedAt: [],
    });
  });

  it("separate hunks report separate ranges", () => {
    const before = join("a", "b", "c", "d", "e", "f");
    const after = join("a", "B", "c", "d", "E", "f");
    expect(diffLines(before, after)).toEqual({
      changed: [
        [2, 2],
        [5, 5],
      ],
      removedAt: [],
    });
  });

  it("append at the end of the file", () => {
    expect(diffLines(join("a", "b"), join("a", "b", "c", "d"))).toEqual({
      changed: [[3, 4]],
      removedAt: [],
    });
  });

  it("everything new when the old text was empty-ish", () => {
    expect(diffLines("", join("a", "b"))).toEqual({
      changed: [[1, 2]],
      removedAt: [],
    });
  });

  it("over budget collapses the middle to one changed block", () => {
    const before = join("same", "a", "b", "c", "d", "same");
    const after = join("same", "w", "x", "y", "z", "same");
    // Budget too small for the 8-op script: the trimmed middle (lines 2–5 of
    // the new text) reports as one block.
    expect(diffLines(before, after, 2)).toEqual({
      changed: [[2, 5]],
      removedAt: [],
    });
  });

  it("interleaved edits stay line-accurate", () => {
    const before = join("fn main() {", "  let x = 1;", "  print(x);", "}");
    const after = join(
      "fn main() {",
      "  let x = 2;",
      "  let y = 3;",
      "  print(x + y);",
      "}",
    );
    expect(diffLines(before, after)).toEqual({
      changed: [[2, 4]],
      removedAt: [],
    });
  });
});
