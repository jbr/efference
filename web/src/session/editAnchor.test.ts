import { describe, expect, it } from "vitest";
import { anchorFromResult, locateAnchor } from "./editAnchor.ts";

/** An edit tool's result as the corpus has them: prose, then `===DIFF===` and
 * a context diff — no `@@` hunk headers, which is why the anchor is content
 * and not line numbers. */
const result = (diff: string) =>
  `Applied insert before — the file has been updated.\n\n===DIFF===\n\n${diff}`;

describe("anchorFromResult", () => {
  it("takes the inserted lines and the context around them", () => {
    const anchor = anchorFromResult(
      result([" fn one() {}", "-    old();", "+    fresh();", " }"].join("\n")),
    );
    expect(anchor).toEqual({
      added: ["    fresh();"],
      before: ["fn one() {}"],
      after: ["}"],
    });
  });

  it("is null for a result with no diff section", () => {
    expect(anchorFromResult("wrote 4 lines")).toBeNull();
  });
});

describe("locateAnchor", () => {
  const file = ["fn one() {}", "    fresh();", "}", "", "fn two() {}"].join(
    "\n",
  );

  it("is exact when the inserted lines are there once", () => {
    const anchor = {
      added: ["    fresh();"],
      before: ["fn one() {}"],
      after: ["}"],
    };
    expect(locateAnchor(anchor, file)).toEqual({
      kind: "exact",
      line: 2,
      changed: [[2, 2]],
    });
  });

  it("still matches when the file was reindented under the edit", () => {
    const anchor = { added: ["fresh();"], before: [], after: [] };
    expect(locateAnchor(anchor, file)).toMatchObject({
      kind: "exact",
      line: 2,
    });
  });

  it("breaks a tie on the context the edit sat between", () => {
    const twice = ["a", "x();", "b", "c", "x();", "d"].join("\n");
    const anchor = { added: ["x();"], before: ["c"], after: ["d"] };
    expect(locateAnchor(anchor, twice)).toMatchObject({
      kind: "exact",
      line: 5,
    });
  });

  it("is approximate when the same lines appear more than once and the context does not decide", () => {
    const twice = ["x();", "x();"].join("\n");
    const anchor = { added: ["x();"], before: [], after: [] };
    expect(locateAnchor(anchor, twice)).toEqual({
      kind: "approximate",
      line: 1,
    });
  });

  it("falls back to the surrounding context, and says the position is a guess", () => {
    // The historical case: a later edit removed what this one inserted. The
    // context still says roughly where — approximate, never a confident
    // highlight in the wrong place.
    const anchor = {
      added: ["    gone();"],
      before: ["fn one() {}"],
      after: [],
    };
    expect(locateAnchor(anchor, file)).toEqual({
      kind: "approximate",
      line: 2,
    });
  });

  it("is unlocated when nothing about the edit is in the file any more", () => {
    const anchor = { added: ["nowhere();"], before: ["gone()"], after: [] };
    expect(locateAnchor(anchor, file)).toEqual({ kind: "unlocated" });
  });

  it("declines an all-blank insertion rather than matching everywhere", () => {
    const anchor = { added: ["   "], before: [], after: [] };
    expect(locateAnchor(anchor, file)).toEqual({ kind: "unlocated" });
  });
});
