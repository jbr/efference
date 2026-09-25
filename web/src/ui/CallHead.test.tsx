// The head's markup is the wrapping rule (docs/web-client.md), so it is what
// this asserts on: everything read as one sentence sits inside the single
// `.call-sig` inline flow. Rendered to static markup rather than through a DOM
// — no layout is being measured, only which element each span is inside.

import { expect, test } from "vitest";
import { renderToStaticMarkup } from "react-dom/server";
import { CallHead } from "./CallHead.tsx";
import { callView } from "./signature.ts";
import { layoutSignature } from "./signatureLayout.ts";

const head = (name: string, input: unknown, columns: number | null = null) =>
  renderToStaticMarkup(
    <CallHead layout={layoutSignature(callView(name, input), columns)} />,
  );

test("a long single-line command flows inside the signature, not beside it", () => {
  const command =
    "cargo test --workspace --all-features -- --nocapture projection::byte_stability";
  const markup = head("bash", { command });
  // One flex item holds prompt, name and command: as siblings they were
  // blockified by the flex container and the command took its own line.
  const sig = markup.slice(markup.indexOf('class="call-sig shell-command"'));
  const end = sig.indexOf("</span></div>");
  expect(sig.slice(0, end)).toContain(command);
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
  expect(markup).toContain(
    '<span class="call-server">ferritin.</span><span class="call-tool">get</span>',
  );
  // A built-in carries no server span at all.
  expect(head("read", { paths: ["a.rs"] })).not.toContain("call-server");
});

test("a content argument is absent from the head, not a shape standing in", () => {
  const markup = head("write", { path: "a.rs", contents: "a\nb\nc" });
  expect(markup).toContain(
    '<span class="call-arg-value">&quot;a.rs&quot;</span>',
  );
  expect(markup).not.toContain("contents");
});

test("a broken signature is a row per line, indented in columns", () => {
  const markup = head(
    "read",
    { paths: ["src/api/provider/listing.rs", "src/boot.rs"] },
    30,
  );
  expect(markup).toContain('class="call-sig broken"');
  // `ch` is the unit the layout counted in.
  expect(markup).toContain('class="call-row" style="padding-left:2ch"');
  // A flat one is a single inline flow with no rows.
  expect(head("read", { paths: ["a.rs"] }, 80)).not.toContain("call-row");
});

test("a shell command reads after a prompt, not the tool's name", () => {
  const markup = head("bash", { command: "ls -la" });
  expect(markup).toContain('class="call-sig shell-command"');
  expect(markup).not.toContain("call-tool");
  // Backgrounding is a tag beside the command, never part of it.
  expect(head("bash", { command: "cargo watch", background: true })).toContain(
    '<span class="call-tag">background</span>',
  );
  // A multi-line command is its block; a head with nothing to tag is empty.
  expect(head("bash", { command: "a\nb" })).toBe("<div></div>");
});
