// Mirrors the behaviors `src/tui/projection/tools.rs` pins (its coverage
// lives in the TUI snapshot harness, `src/tui/harness/test/transcript/
// results.rs`) — the cordon's divergence check, like triage.test.ts.

import { expect, test } from "vitest";
import {
  splitToolName,
  diffRows,
  isDiffTool,
  callArgs,
  langForPath,
  readMarker,
  readRows,
  resultRender,
  byteSize,
  elisionLabel,
} from "./parse.ts";

const ok = (content: string) => ({ content, is_error: false });

test("read_marker classifies BEGIN, END, and content lines", () => {
  expect(readMarker("==tok BEGIN src/foo.rs, LENGTH: 42 tok==")).toEqual({
    path: "src/foo.rs",
  });
  expect(readMarker("==tok BEGIN src/foo.rs, BYTES: 42 tok==")).toEqual({
    path: "src/foo.rs",
  });
  expect(
    readMarker("==tok BEGIN src/foo.rs, BYTES: 42, LINES: 3-4 tok=="),
  ).toEqual({ path: "src/foo.rs" });
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

test("triaged: the parse runs whole, the kept filter applies to its rows", () => {
  // Mirrors transcript_triaged_edit_keeps_diff_rendering: 7 raw lines,
  // one kept — the diff line classification survives the filter.
  const content = "summary\nof edit\n===DIFF===\n@@ -1 +1 @@\n-old\n+new\n ctx";
  const view = resultRender("edit", ok(content), {
    Triaged: { keep: { Ranges: [[6, 6]] }, note: "kept the addition" },
  });
  expect(view.body).toEqual({
    kind: "diff",
    lines: [{ kind: "added", text: "+new" }],
  });
  expect(view.sizeLabel).toBe("kept 1 of 7 lines");
  expect(view.stateWord).toBe("triaged");
  expect(view.note).toBe("kept the addition");
});

test("a selection that kept everything presents like any whole result", () => {
  const content = "one\ntwo\nthree";
  const view = resultRender("bash", ok(content), {
    Triaged: { keep: "All", note: null },
  });
  expect(view.sizeLabel).toBeNull();
  expect(view.stateWord).toBeNull();
  expect(view.body.kind).toBe("numbered");
});

test("triaged non-diff results fall back to numbered kept lines", () => {
  const content = Array.from({ length: 40 }, (_, i) => `line${i + 1}`).join(
    "\n",
  );
  const view = resultRender("bash", ok(content), {
    Triaged: {
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

test("a mounted server's name splits at the first `__`", () => {
  expect(splitToolName("ferritin__search")).toEqual({
    server: "ferritin",
    tool: "search",
  });
  // A built-in has no server half — it is not given an `efference.` prefix.
  expect(splitToolName("read").server).toBeNull();
  // The boundary is the first `__`; a server's own `__` keeps it.
  expect(splitToolName("srv__a__b")).toEqual({ server: "srv", tool: "a__b" });
});

test("diff-tool set and language map mirror the Rust", () => {
  expect(["edit", "undo_edit", "retarget_edit"].every(isDiffTool)).toBe(true);
  expect(isDiffTool("write")).toBe(false);
  expect(langForPath("x/y/z.toml")).toBe("toml");
  expect(langForPath("noext")).toBeNull();
});

// The args tab's own view of the input. The signature's bodies are empty for
// the edit family by design (the diff below the header is the representation), which is
// why the tab used to say "no body — the header says it all" while dropping
// `anchor` and `content` — most of what the call actually asked for.
test("callArgs shows an edit's anchor and content, which the header omits", () => {
  const input = {
    intent: "rename the field",
    file_path: "src/log.rs",
    operation: "replace",
    language: "rust",
    anchor: "fn a() {\n  b();\n}",
    content: "fn a() {\n  c();\n}",
  };
  const fields = callArgs("edit", input);
  // Scalars first — the call's identity — then the blobs, stable within each.
  expect(fields.map((f) => f.key)).toEqual([
    "file_path",
    "operation",
    "language",
    "anchor",
    "content",
  ]);
  expect(fields[0]).toEqual({
    key: "file_path",
    scalar: "src/log.rs",
    code: null,
  });
  // Multi-line strings are code blocks, not escaped JSON scalars: a
  // pretty-print puts source on one line with literal \n and no
  // highlighting.
  expect(fields[3].code).toEqual({
    lang: "rust",
    text: "fn a() {\n  b();\n}",
  });
});

test("callArgs drops intent, which renders as the call's leading line", () => {
  expect(
    callArgs("read", { intent: "look", paths: ["a"] }).map((f) => f.key),
  ).toEqual(["paths"]);
});

test("callArgs falls back to the path's extension when no language is declared", () => {
  const [field] = callArgs("write", {
    path: "x/y.toml",
    contents: "a = 1\nb = 2",
  }).filter((f) => f.key === "contents");
  expect(field.code?.lang).toBe("toml");
});

test("callArgs keeps a multi-line bash command shell, whatever file it names", () => {
  const [field] = callArgs("bash", {
    command: "cd x\nls",
    path: "a.rs",
  }).filter((f) => f.key === "command");
  expect(field.code?.lang).toBe("bash");
});

test("callArgs renders a nested envelope as json and a flat one inline", () => {
  // Declared blob-first; the scalar sorts ahead of it.
  const fields = callArgs("x", { deep: { a: 1, b: 2 }, flat: [] });
  expect(fields[0]).toEqual({ key: "flat", scalar: "[]", code: null });
  expect(fields[1].code?.lang).toBe("json");
});

test("callArgs puts a write's path ahead of its contents, however serialized", () => {
  const fields = callArgs("write", {
    contents: "a\nb",
    create_directories: false,
    path: "web/x.ts",
  });
  expect(fields.map((f) => f.key)).toEqual([
    "create_directories",
    "path",
    "contents",
  ]);
});

// The elision marker mirrors the header `result_entry` composes in
// `src/tui/projection.rs`: same words, same rounding.
test("an elided result is labelled from its tag, not from its content", () => {
  const elided = {
    content: "x".repeat(16 * 1024),
    is_error: false,
    content_elided: { kept_bytes: 16 * 1024, total_bytes: 256 * 1024 },
  };
  expect(elisionLabel(elided)).toBe("16 KiB of 256 KiB shown");
  expect(resultRender("bash", elided, "Raw").elisionLabel).toBe(
    "16 KiB of 256 KiB shown",
  );

  // A whole result carries no marker — absence of the tag is the signal, so a
  // long-but-inline result says nothing about size.
  expect(elisionLabel(ok("x".repeat(64 * 1024)))).toBeNull();
  expect(resultRender("bash", ok("hi"), "Raw").elisionLabel).toBeNull();

  expect(byteSize(900)).toBe("900 B");
  expect(byteSize(1024)).toBe("1 KiB");
});

test("the daemon's selection counts win over a prefix that holds none of the kept lines", () => {
  // A 3-line prefix of a 937-line result whose verdict kept lines 321..338: the
  // local count reads 0 of 3; the daemon's reads 18 of 937 and the body says
  // where the lines are rather than presenting as a verdict that kept nothing.
  const prefix = {
    content: "a\nb\nc",
    is_error: false,
    content_elided: { kept_bytes: 5, total_bytes: 59_670 },
  };
  const wire = {
    Triaged: {
      keep: { Ranges: [[321, 338]] as [number, number][] },
      note: null,
    },
  };
  const view = resultRender("read", prefix, wire, {
    total_lines: 937,
    kept_lines: 18,
  });
  expect(view.sizeLabel).toBe("kept 18 of 937 lines");
  expect(view.stateWord).toBe("triaged");
  expect(view.body).toEqual({
    kind: "text",
    text: "the 18 kept lines lie past the loaded prefix",
  });
  expect(view.elisionLabel).toBe("5 B of 58 KiB shown");

  // Without the field — an older daemon — the local count stands.
  const local = resultRender("read", prefix, wire);
  expect(local.sizeLabel).toBe("kept 0 of 3 lines");
});

test("a pending result shows its raw output under an awaiting-triage label", () => {
  const r = resultRender("bash", ok("a\nb\nc"), "Pending");
  expect(r.stateWord).toBe("awaiting triage");
  expect(r.sizeLabel).toBeNull();
  expect(r.body).toEqual({ kind: "text", text: "a\nb\nc" });
});

test("the verdict's wire re-renders the same raw with kept counts", () => {
  const content = "a\nb\nc\nd";
  const pending = resultRender("bash", ok(content), "Pending");
  const decided = resultRender(
    "bash",
    ok(content),
    { Triaged: { keep: { Ranges: [[1, 2]] }, note: "n" } },
    { total_lines: 4, kept_lines: 2 },
  );
  expect(pending.stateWord).toBe("awaiting triage");
  expect(decided.stateWord).toBe("triaged");
  expect(decided.sizeLabel).toBe("kept 2 of 4 lines");
  expect(decided.note).toBe("n");
});
