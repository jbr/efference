// Mirrors `src/record/project/test/triaged.rs` case for case (the cordon's
// divergence check — see triage.ts). If a case changes there, change it
// here; if it can't be mirrored, the port has drifted.

import { expect, test } from "vitest";
import {
  renderInterruption,
  TRIAGE_LINE_CAP,
  normalize,
  renderTriaged,
  renderHunks,
  rustLines,
  renderTriageFailure,
  wireView,
} from "./triage.ts";

const lines = (s: string) => s.split("\n");

test("render_triaged covers all, none, and ranges", () => {
  const c = "a\nb\nc\nd\ne";
  const all = renderTriaged(c, "All", null);
  expect(all).toBe(c);

  const none = renderTriaged(c, "None", "nothing useful here");
  expect(none).toBe("(output dropped by triage)\n⟢ nothing useful here");

  const ranges = renderTriaged(c, { Ranges: [[2, 3]] }, null);
  expect(ranges).toContain("@@ lines 2-3 @@");
  expect(ranges.split("\n")).toContain("b");
  expect(ranges.split("\n")).toContain("c");
  expect(ranges).not.toContain("@@ lines 1");
  expect(ranges).not.toContain("@@ line 1 ");
});

test("render_triaged truncates past the cap", () => {
  const body = Array.from(
    { length: TRIAGE_LINE_CAP + 5 },
    (_, i) => `line${i + 1}`,
  ).join("\n");
  const out = renderTriaged(body, { Ranges: [[1, 1]] }, null);
  expect(out.split("\n")).toContain("line1");
  expect(out).toContain("5 more lines truncated");

  const past = renderTriaged(
    body,
    { Ranges: [[TRIAGE_LINE_CAP + 2, TRIAGE_LINE_CAP + 4]] },
    null,
  );
  expect(past).not.toContain(`line${TRIAGE_LINE_CAP + 3}`);

  const cut = renderTriaged(body, { Ranges: [[1, 10]] }, null);
  expect(
    cut.endsWith(
      `@@ ${TRIAGE_LINE_CAP + 5} lines total @@\n… [5 more lines truncated before triage]`,
    ),
  ).toBe(true);
});

test("a range spanning everything gets no header", () => {
  const wire = renderHunks(lines("alpha\nbeta\ngamma"), [[1, 3]], null);
  expect(wire).toBe("alpha\nbeta\ngamma");
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
  expect(renderHunks(ls, [], "wrong file")).toBe(
    "(output dropped by triage)\n⟢ wrong file",
  );
  expect(renderHunks(ls, [], null)).toBe("(output dropped by triage)");
  expect(renderHunks([], [], null)).toBe("(nothing kept)");
});

test("a cut tail closes with the line total", () => {
  const body = Array.from({ length: 120 }, (_, i) => `line${i + 1}`).join("\n");
  const wire = renderHunks(lines(body), [[30, 40]], "n");
  expect(wire.startsWith("@@ lines 30-40 @@\nline30\n")).toBe(true);
  expect(wire.endsWith("line40\n@@ 120 lines total @@\n⟢ n")).toBe(true);
  expect(renderHunks(lines(body), [[110, 120]], null)).not.toContain(
    "lines total",
  );
});

test("trailing blank lines count as covered", () => {
  const body =
    Array.from({ length: 100 }, (_, i) => `line${i + 1}`).join("\n") + "\n\n";
  const ls = rustLines(body);
  expect(ls.length).toBe(101);
  const wire = renderHunks(ls, [[1, 100]], null);
  expect(wire).not.toContain("@@");
  expect(
    renderHunks(ls, [[1, 99]], null).endsWith("@@ 101 lines total @@"),
  ).toBe(true);
});

test("intent_feedback follows the note under its own lead", () => {
  const ls = lines("a\nb");
  expect(renderHunks(ls, [[1, 2]], "two lines", "no question to keep")).toBe(
    "a\nb\n⟢ two lines\n⟢ re your intent: no question to keep",
  );
  expect(renderHunks(ls, [], null, "no question to keep")).toBe(
    "(output dropped by triage)\n⟢ re your intent: no question to keep",
  );
  expect(renderHunks(ls, [], null, "  ")).toBe("(output dropped by triage)");
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
    TriageFailed: { why: "triage called no tool (attempt 3 of 3)" },
  });
  expect(failed?.isError).toBe(true);
  expect(failed?.text).toContain(
    "result withheld — triage failed (triage called no tool (attempt 3 of 3))",
  );
  expect(failed?.text).toContain("This is a harness failure");
  expect(failed?.text).toContain("raw output is in the session log");
  expect(failed?.text).not.toContain("ten thousand lines");
  expect(renderTriageFailure("x")).toContain("running it again is safe");
  expect(renderTriageFailure("x")).not.toContain("not recorded");

  const withFeedback = wireView(raw, {
    Triaged: { keep: "None", note: null, intent_feedback: "wrong question" },
  });
  expect(withFeedback?.text).toBe(
    "(output dropped by triage)\n⟢ re your intent: wrong question",
  );
});

// Parity with `render_interruption` (src/record/project/render.rs): the operator reads the
// line the model read.
test("an interruption renders as the wire's line", () => {
  expect(
    renderInterruption({
      by: "session[parent]",
      after_ms: 65_000,
      killed: false,
    }),
  ).toBe(
    "[interrupted by parent (agent) after 1m 5s: the call finished, and its output was withheld]",
  );
  expect(renderInterruption({ by: "session[parent in ~/code/other]" })).toBe(
    "[interrupted by parent (agent in ~/code/other) before its result was read]",
  );
  expect(renderInterruption({ after_ms: 5_000, killed: true })).toBe(
    "[interrupted by the operator after 5s: the command was killed, and its output was withheld]",
  );
  expect(renderInterruption({ by: "mcp[claude-code]" })).toBe(
    "[interrupted by mcp[claude-code] before its result was read]",
  );
});
