// @vitest-environment jsdom
import { act } from "react";
import { createRoot } from "react-dom/client";
import { afterEach, expect, test } from "vitest";
import { UploadSpecimens } from "./UploadSpecimens.tsx";

Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true });
// jsdom has no layout; ProseMirror's scroll requests only need these to finish.
for (const target of [Node.prototype, Range.prototype]) {
  Object.assign(target, {
    getClientRects: () => [],
    getBoundingClientRect: () => new DOMRect(),
  });
}
const cleanups: (() => void)[] = [];
afterEach(() => cleanups.splice(0).forEach((close) => close()));

test("the upload specimen renders every chip state at both widths", async () => {
  const host = document.createElement("div");
  document.body.append(host);
  const root = createRoot(host);
  cleanups.push(() => {
    act(() => root.unmount());
    host.remove();
  });
  await act(async () => root.render(<UploadSpecimens />));
  // The ready fixture answers on a microtask; the store re-renders the chips.
  await act(async () => {});
  const chips = [...host.querySelectorAll<HTMLElement>(".upload-chip")];
  // Six states in the full-width editor and the same six at phone width.
  expect(chips).toHaveLength(12);
  expect(host.querySelectorAll(".upload-chip img")).toHaveLength(2);
  // The never-answering file/image uploads and refused file keep their states;
  // the ready, pasted and long-name files land.
  expect(host.querySelectorAll(".upload-chip.uploading")).toHaveLength(4);
  expect(host.querySelectorAll(".upload-chip.ready")).toHaveLength(6);
  expect(host.querySelectorAll(".upload-chip.failed")).toHaveLength(2);
  // Only the staged paste has an inline action, in each editor.
  expect(host.querySelectorAll(".upload-inline")).toHaveLength(2);
  expect(
    host.querySelector(".upload-chip.failed .upload-detail")?.textContent,
  ).toContain("refused");
});
