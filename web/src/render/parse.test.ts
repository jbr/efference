// Mirrors the behaviors `src/tui/projection/tools.rs` pins (its coverage
// lives in the TUI snapshot harness, `src/tui/harness/test/transcript/
// results.rs`) — the cordon's divergence check, like distill.test.ts.

import { expect, test } from "vitest";
import {
  callView,
  diffRows,
  isDiffTool,
  langForPath,
  readMarker,
  readRows,
  resultRender,
} from "./parse.ts";

const ok = (content: string) => ({ content, is_error: false });

test("read_marker classifies BEGIN, END, and content lines", () => {
  expect(readMarker("==tok BEGIN src/foo.rs, LENGTH: 42 tok==")).toEqual({
    path: "src/foo.rs",
  });
  expect(readMarker("==tok END src/foo.rs tok==")).toEqual({ path: null });
  expect(readMarker("let x = 1;")).toBeNull();
  expect(readMarker("== not a marker")).toBeNull();
});

test("read_rows strips framing, keeps raw line numbers, tags the language", () => {
  const content =
    "==tok BEGIN src/foo.rs, LENGTH: 2 tok==\nfn main() {}\n// done\n==tok END src/foo.rs tok==";
  const rows = readRows(content);
  expect(rows?.lang).toBe("rust");
  expect(rows?.lines).toEqual([
    [2, "fn main() {}"],
    [3, "// done"],
  ]);
  expect(readRows("plain output, no marker")).toBeNull();
});

test("diff_rows classifies by leading char and skips the preamble", () => {
  const content =
    "Edited src/foo.rs\n===DIFF===\n\n@@ -1,2 +1,2 @@\n context\n-old\n+new";
  const rows = diffRows(content);
  expect(rows?.map(([n, l]) => [n, l.kind, l.text])).toEqual([
    [4, "hunk", "@@ -1,2 +1,2 @@"],
    [5, "context", " context"],
    [6, "removed", "-old"],
    [7, "added", "+new"],
  ]);
  expect(diffRows("no marker here")).toBeNull();
  expect(diffRows("prose\n===DIFF===\n\n  \n")).toBeNull();
});

test("a raw diff-tool result renders as a diff; errors stay plain", () => {
  const content = "done\n===DIFF===\n+added line";
  const diff = resultRender("edit", ok(content), "Raw");
  expect(diff.body).toEqual({
    kind: "diff",
    lines: [{ kind: "added", text: "+added line" }],
  });
  expect(diff.stateWord).toBe("raw");

  const err = resultRender("edit", { content, is_error: true }, "Raw");
  expect(err.body.kind).toBe("text");
  expect(err.isError).toBe(true);
});

test("a raw read result drops framing and carries the language hook", () => {
  const content =
    "==tok BEGIN a.py, LENGTH: 1 tok==\nprint(1)\n==tok END a.py tok==";
  const view = resultRender("read", ok(content), "Raw");
  expect(view.body).toEqual({ kind: "code", lang: "python", text: "print(1)" });
});

test("distilled: the parse runs whole, the kept filter applies to its rows", () => {
  // Mirrors transcript_distilled_edit_keeps_diff_rendering: 7 raw lines,
  // one kept — the diff line classification survives the filter.
  const content = "summary\nof edit\n===DIFF===\n@@ -1 +1 @@\n-old\n+new\n ctx";
  const view = resultRender("edit", ok(content), {
    Distilled: { keep: { Ranges: [[6, 6]] }, note: "kept the addition" },
  });
  expect(view.body).toEqual({
    kind: "diff",
    lines: [{ kind: "added", text: "+new" }],
  });
  expect(view.sizeLabel).toBe("kept 1 of 7 lines");
  expect(view.stateWord).toBe("distilled");
  expect(view.note).toBe("kept the addition");
});

test("a distillate that kept everything presents like any whole result", () => {
  const content = "one\ntwo\nthree";
  const view = resultRender("bash", ok(content), {
    Distilled: { keep: "All", note: null },
  });
  expect(view.sizeLabel).toBeNull();
  expect(view.stateWord).toBeNull();
  expect(view.body.kind).toBe("numbered");
});

test("distilled non-diff results fall back to numbered kept lines", () => {
  const content = Array.from({ length: 40 }, (_, i) => `line${i + 1}`).join(
    "\n",
  );
  const view = resultRender("bash", ok(content), {
    Distilled: {
      keep: {
        Ranges: [
          [2, 3],
          [40, 40],
        ],
      },
      note: null,
    },
  });
  expect(view.body).toEqual({
    kind: "numbered",
    lang: null,
    lines: [
      [2, "line2"],
      [3, "line3"],
      [40, "line40"],
    ],
  });
  expect(view.sizeLabel).toBe("kept 3 of 40 lines");
});

test("call views summarize known tools and envelope the rest", () => {
  expect(callView("bash", { command: "ls -la", intent: "peek" })).toEqual({
    intent: "peek",
    summary: "ls -la",
    body: null,
  });
  expect(callView("bash", { command: "a\nb" }).body).toEqual({
    lang: "bash",
    text: "a\nb",
  });
  expect(callView("read", { paths: ["a.rs", "b.rs"] }).summary).toBe(
    "a.rs, b.rs",
  );
  expect(callView("search", { pattern: "foo", path: "src" }).summary).toBe(
    "foo  in  src",
  );
  expect(
    callView("write", { path: "a.rs", contents: "fn x() {}", overwrite: true }),
  ).toEqual({
    intent: null,
    summary: "a.rs · overwrite",
    body: { lang: "rust", text: "fn x() {}" },
  });
  expect(callView("read_image", { path: "mock.png" }).summary).toBe("mock.png");
  expect(
    callView("read_image", { path: "m.png", long_edge: 2576 }).summary,
  ).toBe("m.png · long_edge 2576");
  expect(
    callView("edit", { file_path: "a.rs", operation: "replace" }).summary,
  ).toBe("a.rs · replace");
  expect(callView("mystery", { x: 1 }).body?.lang).toBe("json");
  // The envelope drops `intent` — it already renders as the leading line.
  const enveloped = callView("mystery", { intent: "why", x: 1 });
  expect(enveloped.intent).toBe("why");
  expect(enveloped.body?.text).not.toContain("why");
  expect(enveloped.body?.text).toContain("x");
});

test("diff-tool set and language map mirror the Rust", () => {
  expect(["edit", "undo_edit", "retarget_edit"].every(isDiffTool)).toBe(true);
  expect(isDiffTool("write")).toBe(false);
  expect(langForPath("x/y/z.toml")).toBe("toml");
  expect(langForPath("noext")).toBeNull();
});
