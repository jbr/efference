import { expect, test } from "vitest";
import { callView } from "./signature.ts";
import { layoutSignature, type SigLayout } from "./signatureLayout.ts";

/** The layout as the text it renders, one row per line, indents as spaces. */
const text = (layout: SigLayout) =>
  layout.rows
    .map(
      (row) => " ".repeat(row.indent) + row.tokens.map((t) => t.text).join(""),
    )
    .join("\n");

const at = (columns: number | null, name: string, input: unknown) =>
  layoutSignature(callView(name, input), columns);

test("a call that fits is one line", () => {
  const layout = at(80, "read", { paths: ["a.rs"], from_line: 3 });
  expect(layout.broken).toBe(false);
  expect(text(layout)).toBe('read("a.rs", from_line: 3)');
});

test("before the width is known the signature is flat, and nothing is pulled", () => {
  const long = "x".repeat(500);
  const layout = at(null, "mystery", { note: long });
  expect(layout.broken).toBe(false);
  expect(layout.pulled).toEqual([]);
});

test("a call that does not fit breaks after `(`, one argument per line", () => {
  const layout = at(40, "read", {
    paths: ["src/api/provider/listing.rs", "src/boot.rs"],
    from_line: 3,
  });
  expect(layout.broken).toBe(true);
  expect(text(layout)).toBe(
    [
      "read(",
      '  "src/api/provider/listing.rs",',
      '  "src/boot.rs",',
      "  from_line: 3",
      ")",
    ].join("\n"),
  );
});

test("a lone scalar argument is not broken around — it wraps as text", () => {
  const path = "../codex/codex-rs/model-provider/src/models_cache_ttl.rs";
  const layout = at(30, "read", { paths: [path] });
  expect(layout.broken).toBe(false);
  expect(text(layout)).toBe(`read("${path}")`);
});

test("a collection that does not fit on its row breaks inside, the same way", () => {
  const layout = at(30, "mystery", {
    globs: ["src/**/*.rs", "web/src/**/*.ts"],
    n: 1,
  });
  expect(text(layout)).toBe(
    [
      "mystery(",
      "  globs: [",
      '    "src/**/*.rs",',
      '    "web/src/**/*.ts"',
      "  ],",
      "  n: 1",
      ")",
    ].join("\n"),
  );
  // One that fits on its row stays flat there.
  expect(
    text(at(40, "mystery", { globs: ["a", "b"], other: "x".repeat(20) })),
  ).toBe(
    [
      "mystery(",
      '  globs: ["a", "b"],',
      `  other: "${"x".repeat(20)}"`,
      ")",
    ].join("\n"),
  );
});

test("a value longer than three lines at this width leaves the signature", () => {
  const pattern = "a|".repeat(60); // 120 chars, plus quotes
  // At 100 columns it is well under three lines: it stays, and the call breaks.
  const wide = at(100, "search", { pattern, path: "src" });
  expect(wide.pulled).toEqual([]);
  // At 40 it would take four lines: pulled, and absent from the rows.
  const narrow = at(40, "search", { pattern, path: "src" });
  expect(narrow.pulled.map((arg) => arg.key)).toEqual(["pattern"]);
  expect(text(narrow)).toBe('search(path: "src")');
});

test("a chip's width is budgeted as it renders", () => {
  const view = callView("write", { path: "src/api.rs", overwrite: true });
  // `write("src/api.rs", overwrite: true)` is 36 columns; the chip's ` ⤢` makes 38.
  expect(layoutSignature(view, 36).broken).toBe(false);
  expect(layoutSignature(view, 36, () => 2).broken).toBe(true);
  // The chip rides the scalar value token, which carries its argument.
  const [value] = layoutSignature(view, 80).rows[0].tokens.filter(
    (t) => t.arg !== undefined,
  );
  expect(value.arg?.key).toBe("path");
});

test("bash's command is one row whatever the width — it wraps as text", () => {
  const command = "cargo test --workspace -- --nocapture projection::stable";
  const layout = at(20, "bash", { command });
  expect(layout.rows).toHaveLength(1);
  expect(text(layout)).toBe(`bash · ${command}`);
});
