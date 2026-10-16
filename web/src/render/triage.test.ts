// Mirrors `src/project/test/triaged.rs` case for case (the cordon's
// divergence check — see triage.ts). If a case changes there, change it
// here; if it can't be mirrored, the port has drifted.

import { expect, test } from "vitest";
import {
  PEEK_LINE_CAP,
  normalize,
  renderTriaged,
  renderHunks,
  renderTriageFailure,
  wireView,
} from "./triage.ts";

const lines = (s: string) => s.split("\n");

test("render_triaged covers all, none, and ranges", () => {
  const c = "a\nb\nc\nd\ne";
  const all = renderTriaged(c, "All", null);
  expect(all).toContain("@@ lines 1-5 @@");
  expect(all.split("\n")).toContain("a");
  expect(all.split("\n")).toContain("e");

  const none = renderTriaged(c, "None", "nothing useful here");
  expect(none).toBe("⟢ nothing useful here");

  const ranges = renderTriaged(c, { Ranges: [[2, 3]] }, null);
  expect(ranges).toContain("@@ lines 2-3 @@");
  expect(ranges.split("\n")).toContain("b");
  expect(ranges.split("\n")).toContain("c");
  expect(ranges).not.toContain("@@ lines 1");
  expect(ranges).not.toContain("@@ line 1 ");
});

test("render_triaged truncates past the cap", () => {
  const body = Array.from(
    { length: PEEK_LINE_CAP + 5 },
    (_, i) => `line${i + 1}`,
  ).join("\n");
  const out = renderTriaged(body, { Ranges: [[1, 1]] }, null);
  expect(out.split("\n")).toContain("line1");
  expect(out).toContain("5 more lines truncated");

  const past = renderTriaged(
    body,
    { Ranges: [[PEEK_LINE_CAP + 2, PEEK_LINE_CAP + 4]] },
    null,
  );
  expect(past).not.toContain(`line${PEEK_LINE_CAP + 3}`);
});

test("canonical hunk is verbatim under a diff header", () => {
  const wire = renderHunks(lines("alpha\nbeta\ngamma"), [[1, 3]], null);
  expect(wire).toContain("@@ lines 1-3 @@");
  expect(wire.split("\n")).toContain("alpha");
  expect(wire.split("\n")).toContain("gamma");
});

test("gapped hunks get a header each and drop the gap", () => {
  const body = Array.from({ length: 100 }, (_, i) => `line${i + 1}`).join("\n");
  const wire = renderHunks(
    lines(body),
    [
      [1, 3],
      [99, 100],
    ],
    null,
  );
  expect(wire).toContain("@@ lines 1-3 @@");
  expect(wire).toContain("@@ lines 99-100 @@");
  expect(wire).not.toContain("line50");
  expect(wire.length).toBeLessThan(body.length);
});

test("single-line hunk uses the singular header", () => {
  const wire = renderHunks(lines("a\nb\nc"), [[2, 2]], null);
  expect(wire).toContain("@@ line 2 @@");
  expect(wire.split("\n")).toContain("b");
});

test("note appends and empty falls back to breadcrumb", () => {
  const ls = lines("a\nb\nc");
  expect(renderHunks(ls, [[2, 2]], "the b line").endsWith("⟢ the b line")).toBe(
    true,
  );
  expect(renderHunks(ls, [], "wrong file")).toBe("⟢ wrong file");
  expect(renderHunks(ls, [], null)).toBe("(nothing kept)");
});

test("normalize clamps, sorts, and merges", () => {
  expect(
    normalize(
      [
        [1, 3],
        [2, 5],
      ],
      100,
    ),
  ).toEqual([[1, 5]]);
  expect(
    normalize(
      [
        [1, 3],
        [4, 6],
      ],
      100,
    ),
  ).toEqual([[1, 6]]);
  expect(
    normalize(
      [
        [310, 318],
        [40, 52],
      ],
      400,
    ),
  ).toEqual([
    [40, 52],
    [310, 318],
  ]);
  expect(normalize([[1, 1000]], 100)).toEqual([[1, 100]]);
  expect(normalize([[0, 0]], 100)).toEqual([]);
  expect(normalize([[50, 60]], 10)).toEqual([]);
});

test("wireView dispatches per wire state like tool_result_block", () => {
  const raw = { content: "the gist line\nten thousand lines", is_error: false };
  expect(wireView(raw, "Raw")).toBeNull();
  expect(wireView(raw, "Elided")).toEqual({ text: "[elided]", isError: false });

  const triaged = wireView(raw, {
    Triaged: { keep: { Ranges: [[1, 1]] }, note: "summary" },
  });
  expect(triaged?.text).toContain("the gist line");
  expect(triaged?.text).toContain("⟢ summary");
  expect(triaged?.text).not.toContain("ten thousand lines");

  const failed = wireView(raw, {
    TriageFailed: { why: "peek called no tool (attempt 3 of 3)" },
  });
  expect(failed?.isError).toBe(true);
  expect(failed?.text).toContain(
    "result withheld — triage failed (peek called no tool (attempt 3 of 3))",
  );
  expect(failed?.text).toContain("ran to completion");
  expect(failed?.text).not.toContain("ten thousand lines");
  expect(renderTriageFailure("x")).toContain("Re-issue the call");
});
