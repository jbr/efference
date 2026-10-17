import { expect, test } from "vitest";
import { callView } from "./signature.ts";
import { flatSignature } from "./signatureLayout.ts";

const sig = (name: string, input: unknown) =>
  flatSignature(callView(name, input));

test("a mounted server's call is dotted, with the server its own segment", () => {
  const view = callView("ferritin__search", {
    crate: "serde",
    query: "Deserialize",
  });
  // The wire's `__` never shows: the dotted form is the only spelling an
  // operator ever writes.
  expect(view.server).toBe("ferritin");
  expect(view.tool).toBe("search");
  // A mounted tool's arguments keep their keys, whatever it is called.
  expect(flatSignature(view)).toBe(
    'ferritin.search(crate: "serde", query: "Deserialize")',
  );
});

test("a call with no arguments is empty, not an envelope of `{}`", () => {
  const view = callView("check_inbox", {});
  expect(view.args).toEqual([]);
  expect(view.bodies).toEqual([]);
  expect(flatSignature(view)).toBe("check_inbox()");
  // Nor does an intent-only call have arguments: the intent is its own line.
  expect(sig("list", { intent: "look around" })).toBe("list()");
});

test("a built-in's primary argument reads positionally, an array spread", () => {
  expect(sig("read", { paths: ["a.rs", "b.rs"], from_line: 3 })).toBe(
    'read("a.rs", "b.rs", from_line: 3)',
  );
  // Positional first, whatever order the input serialized in.
  expect(sig("search", { path: "src", pattern: "foo" })).toBe(
    'search("foo", path: "src")',
  );
  expect(sig("read_image", { path: "m.png", long_edge: 2576 })).toBe(
    'read_image("m.png", long_edge: 2576)',
  );
  // The key is kept on the argument, for the chip table.
  expect(callView("read", { paths: ["a.rs"] }).args).toEqual([
    { key: "paths", label: null, value: "a.rs" },
  ]);
  // A mounted tool of the same name is not a built-in.
  expect(sig("srv__read", { paths: ["a.rs"] })).toBe(
    'srv.read(paths: ["a.rs"])',
  );
});

test("every single-line argument stays in the signature, whatever its length", () => {
  // Whether one is too long to sit there is the layout's call, at a width.
  const long = "x".repeat(200);
  const view = callView("mystery", {
    note: long,
    region: { x: 1, y: 2 },
  });
  expect(view.bodies).toEqual([]);
  expect(flatSignature(view)).toBe(
    `mystery(note: "${long}", region: { x: 1, y: 2 })`,
  );
});

test("a multi-line value is content: gone from the signature, a body below", () => {
  const contents = Array.from({ length: 505 }, (_, i) => `line ${i}`).join(
    "\n",
  );
  const view = callView("write", {
    intent: "lay it down",
    contents,
    path: "src/main.rs",
    overwrite: true,
  });
  expect(flatSignature(view)).toBe('write("src/main.rs", overwrite: true)');
  expect(view.intent).toBe("lay it down");
  // Verbatim, line breaks and all, in the language of the path it writes.
  expect(view.bodies).toEqual([
    { key: "contents", lang: "rust", text: contents },
  ]);
  // A collection holding a multi-line string is content too.
  const nested = callView("mystery", { item: { body: "a\nb" }, n: 1 });
  expect(flatSignature(nested)).toBe("mystery(n: 1)");
  expect(nested.bodies[0].lang).toBe("json");
});

test("bash keeps its command bare — a shell line, not a quoted argument", () => {
  const view = callView("bash", { command: "ls -la", intent: "peek" });
  expect(view).toEqual({
    intent: "peek",
    server: null,
    tool: "bash",
    args: null,
    target: "ls -la",
    shell: { background: false, backgroundAfter: null },
    bodies: [],
  });
  expect(
    callView("bash", { command: "make", background_after: 600 }).shell,
  ).toEqual({ background: false, backgroundAfter: 600 });
  // Only a newline makes it a block.
  expect(callView("bash", { command: "a\nb" }).bodies).toEqual([
    { key: null, lang: "bash", text: "a\nb" },
  ]);
});

test("the edit family omits its content and emits no body — the diff is below", () => {
  const view = callView("edit", {
    file_path: "a.rs",
    operation: "replace",
    anchor: "fn a() {\n  b();\n}",
  });
  expect(flatSignature(view)).toBe('edit("a.rs", operation: "replace")');
  expect(view.bodies).toEqual([]);
});

// The brief is a message to the session that takes over (docs/tools.md).
test("a hand-off puts the brief under whom it is for, as prose", () => {
  const view = callView("agents__hand_off", {
    brief: "Greet the operator.",
    intent: "hand off",
  });
  expect(view.args).toEqual([]);
  expect(view.bodies).toEqual([
    {
      key: "to the new session",
      lang: null,
      text: "Greet the operator.",
      prose: true,
    },
  ]);
});
