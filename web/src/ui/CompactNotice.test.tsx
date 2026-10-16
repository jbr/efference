// @vitest-environment jsdom
// The composer's cold notice (docs/web-client.md): the compaction offered beside the
// uncached read, and a compaction's progress and failure.

import { expect, test, vi } from "vitest";
import { act } from "react";
import { createRoot } from "react-dom/client";
import type { Cold, CurrentOperations, Verbs } from "../api/types.ts";
import { CompactNotice } from "./CompactNotice.tsx";
import { compactOffer } from "./compact.ts";

vi.hoisted(() => {
  Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true });
});

const cold: Cold = {
  since: 1_754_300_000_000,
  context_tokens: 120_400,
};

const verbs = (compact: Verbs["compact"]): Verbs => ({
  turn: { accepted: true, delivery: "now" },
  continue: { accepted: true },
  compact,
});
const ready = verbs({ accepted: true, warm: false });

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
      failure={null}
      offer={compactOffer(ready, null, cold)}
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
  const writing: CurrentOperations = {
    requests: [],
    tools: [],
    compaction: { state: "writing", warm: false, cut: 4, since: 1 },
  };
  const host = render(
    <CompactNotice
      cold={cold}
      failure={null}
      offer={compactOffer(ready, writing, cold)}
      onCompact={() => {}}
    />,
  );
  expect(host.textContent).toContain("summarizing the history");
  expect(host.querySelector("button")).toBeNull();
});

test("a failure states its error and offers another try", () => {
  const failure = { error: "upstream 529", at: 1 };
  const host = render(
    <CompactNotice
      cold={cold}
      failure={failure}
      offer={compactOffer(ready, null, cold)}
      onCompact={() => {}}
    />,
  );
  expect(host.textContent).toContain("summary failed: upstream 529");
  expect(host.querySelector("button")?.textContent).toBe(
    "try summarizing again",
  );
});

test("a compaction the route refuses gives its reason instead of a button", () => {
  const host = render(
    <CompactNotice
      cold={cold}
      failure={null}
      offer={compactOffer(
        verbs({
          accepted: false,
          message: "an approval gate is pending — resolve it first",
        }),
        null,
        cold,
      )}
      onCompact={() => {}}
    />,
  );
  expect(host.querySelector("button")).toBeNull();
  expect(host.textContent).toContain("an approval gate is pending");
});
