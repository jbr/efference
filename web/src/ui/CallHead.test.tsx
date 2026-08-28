// The head's markup is the wrapping rule (docs/web-client.md), so it is what
// this asserts on: everything read as one sentence sits inside the single
// `.call-sig` inline flow. Rendered to static markup rather than through a DOM
// — no layout is being measured, only which element each span is inside.

import { expect, test } from "vitest";
import { renderToStaticMarkup } from "react-dom/server";
import { callView } from "../render/parse.ts";
import { CallHead } from "./CallHead.tsx";

const head = (name: string, input: unknown) =>
  renderToStaticMarkup(<CallHead view={callView(name, input)} />);

test("a long single-line command flows inside the signature, not beside it", () => {
  const command =
    "cargo test --workspace --all-features -- --nocapture projection::byte_stability";
  const markup = head("bash", { command });
  // One flex item holds prompt, name and command: as siblings they were
  // blockified by the flex container and the command took its own line.
  const sig = markup.slice(markup.indexOf('class="call-sig"'));
  const end = sig.indexOf("</span></div>");
  expect(sig.slice(0, end)).toContain(command);
  expect(markup).not.toContain('</span><span class="call-target"');
  // And the command is verbatim — `.call-target` is `white-space: pre-wrap`.
  expect(markup).toContain(`>${command}<`);
});

test("the head holds the signature and nothing beside it", () => {
  const markup = head("write", { path: "a.rs" });
  // One flex-free block with one child: affordances ride their own argument
  // now (chips.tsx), so there is no slot at the end of the row.
  expect(
    markup.startsWith('<div class="call-head"><span class="call-sig">'),
  ).toBe(true);
  expect(markup.endsWith("</span></div>")).toBe(true);
});

test("the server segment is its own span, so it can be dimmed", () => {
  const markup = head("ferritin__get", { path: "serde::Deserialize" });
  expect(markup).toContain('<span class="call-server">ferritin.</span>get');
  // A built-in carries no server span at all.
  expect(head("read", { paths: ["a.rs"] })).not.toContain("call-server");
});

test("an elided argument's shape is marked apart from a value", () => {
  const markup = head("write", { path: "a.rs", contents: "a\nb\nc" });
  expect(markup).toContain(
    '<span class="call-arg-value">&quot;a.rs&quot;</span>',
  );
  expect(markup).toContain('<span class="call-arg-shape">3 lines</span>');
});
