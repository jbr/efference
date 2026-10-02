// A result's head and box while triage is pending, rendered to static markup:
// the label and the bounded box are what distinguish the slot from a decided one.

import { expect, test, vi } from "vitest";
import { renderToStaticMarkup } from "react-dom/server";

// The width query reads `matchMedia`, which a server render has no snapshot of.
vi.mock("./useNarrow.ts", () => ({ useNarrow: () => false }));

const { ResultBlock } = await import("./ToolBlocks.tsx");

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
