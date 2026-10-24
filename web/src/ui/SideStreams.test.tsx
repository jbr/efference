// @vitest-environment jsdom
import { afterEach, expect, test, vi } from "vitest";
import { act } from "react";
import { createRoot } from "react-dom/client";
import { SummaryPreview, TriagePreview } from "./SideStreams.tsx";
import { setViewSetting } from "./viewSettings.ts";

vi.hoisted(() => {
  Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true });
  Object.defineProperty(globalThis, "localStorage", {
    configurable: true,
    value: { getItem: () => null, setItem: () => {} },
  });
});

function markup(element: React.ReactElement): string {
  const host = document.createElement("div");
  const root = createRoot(host);
  act(() => root.render(element));
  const html = host.innerHTML;
  act(() => root.unmount());
  return html;
}

afterEach(() => setViewSetting("thinking", true));

test("triage thinking and arriving verdict have distinct surfaces", () => {
  const html = markup(
    <TriagePreview
      tails={[
        {
          callIds: ["a"],
          thinking: "Inspecting the result",
          text: "raw verdict",
        },
      ]}
    />,
  );
  expect(html).toContain("Triaging the result…");
  expect(html).toContain("Inspecting the result");
  expect(html).toContain('<details class="triage-verdict">');
  expect(html).toContain("Arriving verdict");
  expect(html).toContain("raw verdict");
});

test("idle triage is labeled and thinking respects the view preference", () => {
  setViewSetting("thinking", false);
  const html = markup(
    <TriagePreview
      tails={[
        { callIds: [], thinking: "Hidden thinking", text: "visible verdict" },
      ]}
    />,
  );
  expect(html).toContain("Triaging the idle results…");
  expect(html).not.toContain("Hidden thinking");
  expect(html).toContain("visible verdict");
});

test("summary preview labels the frozen span and renders its streamed account", () => {
  const html = markup(
    <SummaryPreview
      node={{
        kind: "summary-preview",
        key: "preview",
        source: "consolidation",
        from: 2,
        upto: 7,
        thinking: "Checking history",
        text: "**The account**",
      }}
    />,
  );
  expect(html).toContain("consolidating");
  expect(html).toContain("#2–#7");
  expect(html).toContain("Checking history");
  expect(html).toContain("<strong>The account</strong>");
});
