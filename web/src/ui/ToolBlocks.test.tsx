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
  expect(markup).toContain('class="line-body">line one');
  expect(markup).toContain('class="line-body">line two');
  expect(markup).toContain('class="range-content range-transcript"');
});

test("a decided result has neither the label nor the box", () => {
  const markup = renderToStaticMarkup(
    <ResultBlock tool="bash" seq={2} raw={raw} wire="Raw" />,
  );
  expect(markup).not.toContain("awaiting triage");
  expect(markup).not.toContain("t-pinned");
});

test("a result image renders as a button that opens the lightbox", () => {
  const markup = renderToStaticMarkup(
    <ResultBlock
      tool="mcp__playwright__browser_take_screenshot"
      seq={7}
      raw={{
        content: "",
        is_error: false,
        images: [{ media_type: "image/png", width: 1179, height: 2556 }],
      }}
      wire="Raw"
    />,
  );
  expect(markup).toContain('class="result-image-button"');
  expect(markup).toContain("/v1/log/7/images/0");
  expect(markup).toContain("1179×2556");
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

test("pending and triaged results use the same line gutter", () => {
  for (const wire of [
    "Pending",
    { Triaged: { keep: "All", note: null } },
  ] as const) {
    const markup = renderToStaticMarkup(
      <ResultBlock tool="bash" seq={2} raw={raw} wire={wire} />,
    );
    expect(markup.match(/class="lineno"/g)).toHaveLength(2);
    expect(markup.match(/class="line-body"/g)).toHaveLength(2);
  }
});

test("short results display whole; longer results start with ten expandable rows", () => {
  for (const count of [14, 20, 21]) {
    const markup = renderToStaticMarkup(
      <ResultBlock
        tool="bash"
        seq={2}
        raw={{
          content: Array.from({ length: count }, (_, i) => `row ${i + 1}`).join(
            "\n",
          ),
          is_error: false,
        }}
        wire={{ Triaged: { keep: "All", note: null } }}
      />,
    );
    expect(markup.match(/class="line-body"/g)).toHaveLength(
      count <= 20 ? count : 10,
    );
    expect(markup.includes('class="range-gap"')).toBe(count > 20);
  }
});

test("read ranges strip sentinels but retain raw verdict coordinates", () => {
  const markup = renderToStaticMarkup(
    <ResultBlock
      tool="read"
      seq={2}
      raw={{
        content:
          "==token BEGIN example.ts, LENGTH: 2==\nconst a = 1;\nconst b = 2;\n==token END example.ts==",
        is_error: false,
      }}
      wire={{ Triaged: { keep: { Ranges: [[3, 3]] }, note: null } }}
    />,
  );
  expect(markup).toContain('data-line="3"');
  expect(markup).toContain("const b = 2;");
  expect(markup).not.toContain("const a = 1;");
  expect(markup).not.toContain("BEGIN example");
  expect(markup).toContain('class="range-gap"');
});

test("edit ranges preserve diff classification and raw coordinates", () => {
  const markup = renderToStaticMarkup(
    <ResultBlock
      tool="undo_edit"
      seq={2}
      raw={{ content: "changed\n===DIFF===\n-old\n+new", is_error: false }}
      wire={{ Triaged: { keep: { Ranges: [[4, 4]] }, note: null } }}
    />,
  );
  expect(markup).toContain('data-line="4"');
  expect(markup).toContain("diff-added");
  expect(markup).not.toContain("===DIFF===");
});
