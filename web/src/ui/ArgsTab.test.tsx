// The args tab's markup: what a reader sees of the schema join, and — the load-
// bearing one — that a mounted server's prose reaches the page as text, not as
// markup. Rendered to static markup rather than through a DOM: nothing here is
// measured, only which element each string lands in.

import { expect, test, vi } from "vitest";
import { renderToStaticMarkup } from "react-dom/server";
import type { ToolEntry } from "../api/types.ts";

// The definition is normally fetched; the tab under test is the join and the
// rendering, so the fetch is stubbed and the entry handed over directly.
let entry: ToolEntry | null = null;
vi.mock("./toolDefinition.ts", () => ({
  useToolEntry: () => entry,
}));
// Shiki's highlighter is async and irrelevant here: a code block renders as
// its text either way.
vi.mock("./Code.tsx", () => ({
  Code: ({ text }: { text: string }) => <pre>{text}</pre>,
}));

const { ArgsTab } = await import("./ArgsTab.tsx");

const schema = {
  type: "object",
  properties: {
    intent: { type: "string", description: "why" },
    path: {
      type: "string",
      description: "Item path, e.g. `serde::Deserialize`.",
    },
    crate_name: { type: "string", description: "The crate to look in." },
  },
  required: ["intent", "path"],
};

const tab = (name: string, input: unknown, intent: string | null = null) =>
  renderToStaticMarkup(<ArgsTab name={name} input={input} intent={intent} />);

test("the tool leads the tab with its dotted name and description", () => {
  entry = {
    name: "ferritin__get",
    description: "Show documentation for a Rust item by path.",
    input_schema: schema,
    server: "ferritin",
  };
  const markup = tab("ferritin__get", { path: "Vec" });

  expect(markup).toContain('<span class="call-server">ferritin.</span>get');
  expect(markup).toContain("Show documentation for a Rust item by path.");
  // The argument's own gloss rides its value.
  expect(markup).toContain("Item path, e.g. `serde::Deserialize`.");
  expect(markup).toContain('<span class="arg-required">required</span>');
});

test("a mounted server's prose is plain text, attributed to the server", () => {
  entry = {
    name: "ferritin__get",
    description: "**bold** and <script>alert(1)</script> and `code`",
    input_schema: schema,
    server: "ferritin",
  };
  const markup = tab("ferritin__get", { path: "Vec" });

  // Verbatim, escaped, inside the plain-text container — no <strong>, no
  // executable markup: this is third-party content shown to a human.
  expect(markup).toContain(
    '<div class="tool-prose plain-text">**bold** and &lt;script&gt;alert(1)&lt;/script&gt; and `code`</div>',
  );
  expect(markup).not.toContain("<strong>");
  expect(markup).toContain("described by ferritin");
});

test("a built-in carries no attribution line", () => {
  entry = {
    name: "bash",
    description: "Run a bash command.",
    input_schema: {
      type: "object",
      properties: { command: { type: "string" } },
    },
  };
  const markup = tab("bash", { command: "ls" });
  expect(markup).toContain("Run a bash command.");
  expect(markup).not.toContain("described by");
});

test("the two mismatches are visible: undeclared sent, declared absent", () => {
  entry = {
    name: "ferritin__get",
    input_schema: schema,
    server: "ferritin",
  };
  const markup = tab("ferritin__get", { crate: "std" });

  expect(markup).toContain('<span class="arg-undeclared"');
  expect(markup).toContain("not in schema");
  expect(markup).toContain("declared, not passed");
  // The missing one is required, and says so more loudly.
  expect(markup).toContain(
    '<span class="arg-required missing">required</span>',
  );
});

test("with no definition the tab is exactly the sent arguments", () => {
  entry = null;
  const markup = tab("ferritin__get", { path: "Vec", crate: "std" }, "why");

  expect(markup).toContain('<div class="intent">why</div>');
  expect(markup).toContain("path");
  expect(markup).toContain("Vec");
  // No tool heading, no flags, no absent section, and no error surface.
  expect(markup).not.toContain("tool-def");
  expect(markup).not.toContain("arg-undeclared");
  expect(markup).not.toContain("args-absent");
  expect(markup.toLowerCase()).not.toContain("error");
});

test("a call with no arguments says so", () => {
  entry = null;
  expect(tab("check_inbox", {})).toContain("this call took no arguments");
});
