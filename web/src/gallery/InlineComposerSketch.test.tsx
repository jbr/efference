// @vitest-environment jsdom
import { act } from "react";
import { createRoot } from "react-dom/client";
import { afterEach, expect, test } from "vitest";
import type { UserContent } from "../api/types.ts";
import { InlineComposerSketch } from "./InlineComposerSketch.tsx";

Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true });
// jsdom has no layout. These geometry methods only allow history's scroll
// requests to finish; caret placement is checked in the live browser.
Object.assign(Range.prototype, {
  getClientRects: () => [],
  getBoundingClientRect: () => new DOMRect(),
});
const cleanups: (() => void)[] = [];
afterEach(() => cleanups.splice(0).forEach((close) => close()));
async function mount() {
  const host = document.createElement("div");
  document.body.append(host);
  const root = createRoot(host);
  await act(async () => root.render(<InlineComposerSketch />));
  cleanups.push(() => {
    act(() => root.unmount());
    host.remove();
  });
  async function click(label: string) {
    const button = [...host.querySelectorAll("button")].find(
      (button) =>
        button.textContent === label ||
        button.getAttribute("aria-label") === label,
    )!;
    await act(async () => button.click());
  }
  const draft = () =>
    JSON.parse(host.querySelector("pre")!.textContent!) as UserContent;
  return { host, click, document: draft };
}

test("inline editor exports atomic reference occurrences and authoritative definitions", async () => {
  const { host, click, document } = await mount();
  await click("Insert file at caret");
  const draft = document();
  const [id] = Object.keys(draft.references);
  expect(draft.content).toContainEqual({ Reference: id });
  expect(draft.references[id].target).toMatchObject({
    type: "filesystem",
    path: "/fixture/web/src/main.tsx",
    access: { read: true, write: false },
  });
  expect(
    host.querySelector(".inline-composer-editor .reference-chip"),
  ).not.toBeNull();
  await click("Allow write access to main.tsx");
  expect(document().references[id].target).toMatchObject({
    access: { read: true, write: true },
  });
});

test("removal prunes unused definitions and undo restores the original identity", async () => {
  const { click, document } = await mount();
  // The fixture carries its own reference; start from an empty document so
  // the assertion measures the inserted chip alone.
  await click("Clear");
  await click("Insert file at caret");
  const [id] = Object.keys(document().references);
  await click("Remove main.tsx");
  expect(document().references).toEqual({});
  await click("Undo");
  expect(Object.keys(document().references)).toEqual([id]);
});

test("Markdown source stays literal without formatting controls", async () => {
  const { host, click, document } = await mount();
  expect(host.querySelector(".inline-composer-editor strong")).toBeNull();
  await click("Insert agent at caret");
  expect(
    document().content.some(
      (part) =>
        "Text" in part && part.Text.includes("\n**Markdown stays literal.**"),
    ),
  ).toBe(true);
  await click("Clear");
  expect(document()).toEqual({ content: [], references: {} });
});
