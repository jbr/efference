// A result's head and box while triage is pending, rendered to static markup:
// the label and the bounded box are what distinguish the slot from a decided one.

import { expect, test, vi } from "vitest";
import { renderToStaticMarkup } from "react-dom/server";

// The width query reads `matchMedia`, which a server render has no snapshot of.
vi.mock("./useNarrow.ts", () => ({ useNarrow: () => false }));

const { ResultBlock, LiveCallBlock } = await import("./ToolBlocks.tsx");

const raw = { content: "line one\nline two", is_error: false };

test("a pending result renders raw output in the bounded box under the label", () => {
  const markup = renderToStaticMarkup(
    <ResultBlock tool="bash" seq={2} raw={raw} wire="Pending" />,
  );
  expect(markup).toContain("result (awaiting triage)");
  expect(markup).toContain("t-pinned");
  expect(markup).toContain("line one\nline two");
});

test("a decided result has neither the label nor the box", () => {
  const markup = renderToStaticMarkup(
    <ResultBlock tool="bash" seq={2} raw={raw} wire="Raw" />,
  );
  expect(markup).not.toContain("awaiting triage");
  expect(markup).not.toContain("t-pinned");
});

test("an unfinished scalar input uses the wrapping content body", () => {
  const fragment =
    "async page => { await page.setViewportSize({width:390,height";
  const markup = renderToStaticMarkup(
    <LiveCallBlock
      name="playwright__browser_run_code_unsafe"
      input={`{"intent":"Check phone width","code":"${fragment}`}
    />,
  );
  expect(markup).toContain('class="call-body-text dim"');
  expect(markup).toContain(
    "code: async page =&gt; { await page.setViewportSize({width:390,height…",
  );
});
