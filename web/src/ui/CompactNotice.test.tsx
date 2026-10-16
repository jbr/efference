// @vitest-environment jsdom
// The composer's cold notice (docs/web-client.md): the compaction offered beside the
// uncached read, and a compaction's progress and failure.

import { expect, test, vi } from "vitest";
import { act } from "react";
import { createRoot } from "react-dom/client";
import type { Cold } from "../api/types.ts";
import { CompactNotice } from "./CompactNotice.tsx";
import { compactOffer } from "./compact.ts";

vi.hoisted(() => {
  Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true });
});

const cold: Cold = {
  since: 1_754_300_000_000,
  context_tokens: 120_400,
};

function render(element: React.ReactElement): HTMLElement {
  const host = document.createElement("div");
  const root = createRoot(host);
  act(() => root.render(element));
  return host;
}

test("a cold session states the read and offers the summary beside it, and the button compacts", () => {
  const onCompact = vi.fn();
  const host = render(
    <CompactNotice
      cold={cold}
      compaction={null}
      offer={compactOffer(cold, null, false, false, false)}
      onCompact={onCompact}
    />,
  );
  expect(host.textContent).toContain(
    "cache expired — a send reads ~120k tokens uncached, or summarize instead",
  );
  act(() => host.querySelector("button")!.click());
  expect(onCompact).toHaveBeenCalledOnce();
});

test("a compaction in flight replaces the choice with its progress", () => {
  const compaction = { state: "writing" } as const;
  const host = render(
    <CompactNotice
      cold={cold}
      compaction={compaction}
      offer={compactOffer(cold, compaction, false, false, false)}
      onCompact={() => {}}
    />,
  );
  expect(host.textContent).toContain("summarizing the history");
  expect(host.querySelector("button")).toBeNull();
});

test("a failure states its error and offers another try", () => {
  const compaction = { state: "failed", error: "upstream 529" } as const;
  const host = render(
    <CompactNotice
      cold={cold}
      compaction={compaction}
      offer={compactOffer(cold, compaction, false, false, false)}
      onCompact={() => {}}
    />,
  );
  expect(host.textContent).toContain("summary failed: upstream 529");
  expect(host.querySelector("button")?.textContent).toBe(
    "try summarizing again",
  );
});

test("a blocked compaction gives the reason instead of a button", () => {
  const host = render(
    <CompactNotice
      cold={cold}
      compaction={null}
      offer={compactOffer(cold, null, false, true, false)}
      onCompact={() => {}}
    />,
  );
  expect(host.querySelector("button")).toBeNull();
  expect(host.textContent).toContain("an approval gate is pending");
});
