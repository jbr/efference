// @vitest-environment jsdom
// The composer's cold notice (docs/web-client.md): the fold offered beside the
// uncached read, and a fold's progress and failure.

import { expect, test, vi } from "vitest";
import { act } from "react";
import { createRoot } from "react-dom/client";
import type { Cold } from "../api/types.ts";
import { FoldNotice } from "./FoldNotice.tsx";
import { foldOffer } from "./fold.ts";

vi.hoisted(() => {
  Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true });
});

const cold: Cold = { since: 1_754_300_000_000, context_tokens: 120_400 };

function render(element: React.ReactElement): HTMLElement {
  const host = document.createElement("div");
  const root = createRoot(host);
  act(() => root.render(element));
  return host;
}

test("a cold refusal offers the summary beside the read, and the button folds", () => {
  const onFold = vi.fn();
  const host = render(
    <FoldNotice
      cold={cold}
      consent
      fold={null}
      offer={foldOffer(cold, null, false, false, false)}
      onFold={onFold}
    />,
  );
  expect(host.textContent).toContain(
    "send again to read 120k tokens uncached, or summarize instead",
  );
  act(() => host.querySelector("button")!.click());
  expect(onFold).toHaveBeenCalledOnce();
});

test("a fold in flight replaces the choice with its progress", () => {
  const fold = { state: "writing" } as const;
  const host = render(
    <FoldNotice
      cold={cold}
      consent
      fold={fold}
      offer={foldOffer(cold, fold, false, false, false)}
      onFold={() => {}}
    />,
  );
  expect(host.textContent).toContain("summarizing the history");
  expect(host.querySelector("button")).toBeNull();
});

test("a failure states its error and offers another try", () => {
  const fold = { state: "failed", error: "upstream 529" } as const;
  const host = render(
    <FoldNotice
      cold={cold}
      consent
      fold={fold}
      offer={foldOffer(cold, fold, false, false, false)}
      onFold={() => {}}
    />,
  );
  expect(host.textContent).toContain("summary failed: upstream 529");
  expect(host.querySelector("button")?.textContent).toBe(
    "try summarizing again",
  );
});

test("a blocked fold gives the reason instead of a button", () => {
  const host = render(
    <FoldNotice
      cold={cold}
      consent
      fold={null}
      offer={foldOffer(cold, null, false, true, false)}
      onFold={() => {}}
    />,
  );
  expect(host.querySelector("button")).toBeNull();
  expect(host.textContent).toContain("an approval gate is pending");
});
