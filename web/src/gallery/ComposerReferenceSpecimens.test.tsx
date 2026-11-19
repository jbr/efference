// @vitest-environment jsdom
import { act } from "react";
import { createRoot } from "react-dom/client";
import { afterEach, expect, test } from "vitest";
import { CompletionSketch } from "./ComposerReferenceSpecimens.tsx";

Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true });
const cleanups: (() => void)[] = [];
afterEach(() => cleanups.splice(0).forEach((close) => close()));

function mount() {
  const host = document.createElement("div");
  document.body.append(host);
  const root = createRoot(host);
  act(() => root.render(<CompletionSketch />));
  cleanups.push(() => {
    act(() => root.unmount());
    host.remove();
  });
  const input = host.querySelector("textarea")!;
  function key(key: string, isComposing = false) {
    const event = new KeyboardEvent("keydown", {
      key,
      isComposing,
      bubbles: true,
      cancelable: true,
    });
    act(() => {
      input.dispatchEvent(event);
    });
    return event;
  }
  return { host, input, key };
}

test("Tab opens a folder, then inserts the selected file reference", () => {
  const { host, input, key } = mount();
  expect(key("Tab").defaultPrevented).toBe(true);
  expect(input.value).toBe("Please check ./web/src/");
  expect(host.querySelector(".reference-chip")).toBeNull();
  expect(host.querySelector('[aria-selected="true"]')?.textContent).toBe(
    "/home/demo/project/web/src",
  );
  key("ArrowDown");
  expect(host.querySelector('[aria-selected="true"]')?.textContent).toBe(
    "main.tsx",
  );
  expect(key("Tab").defaultPrevented).toBe(true);
  expect(host.querySelector(".reference-chip-label")?.textContent).toBe(
    "main.tsx",
  );
  expect(host.querySelector('[role="listbox"]')).toBeNull();
});

test("Click opens the folder; Enter inserts the highlighted folder action without sending", () => {
  const { host, key } = mount();
  act(() => host.querySelector<HTMLButtonElement>('[role="option"]')!.click());
  expect(host.querySelector(".reference-chip")).toBeNull();
  expect(key("Enter").defaultPrevented).toBe(true);
  expect(host.querySelector(".reference-chip-label")?.textContent).toBe("src/");
  expect(host.querySelector('[role="status"]')?.textContent).toContain(
    "Nothing was sent",
  );
});

test("Escape leaves the browsed folder as editable text", () => {
  const { host, input, key } = mount();
  key("Enter");
  key("Escape");
  expect(input.value).toBe("Please check ./web/src/");
  expect(host.querySelector(".reference-chip")).toBeNull();
});

test("IME confirmation does not accept a candidate", () => {
  const { host, key } = mount();
  expect(key("Enter", true).defaultPrevented).toBe(false);
  expect(host.querySelector('[role="listbox"]')).not.toBeNull();
  expect(host.querySelector(".reference-chip")).toBeNull();
});

test("accepted references expose additive write and removal controls", () => {
  const { host, key } = mount();
  key("Enter");
  key("Enter");
  const access = host.querySelector<HTMLButtonElement>(
    ".reference-chip-access",
  )!;
  const name = access.getAttribute("aria-label");
  expect(name).toMatch(/^Allow write access to /);
  expect(access.title).toBe("Read only");
  expect(access.getAttribute("aria-pressed")).toBe("false");
  act(() => access.click());
  expect(access.title).toBe("Read and write");
  expect(access.getAttribute("aria-pressed")).toBe("true");
  expect(access.getAttribute("aria-label")).toBe(name);
  act(() => access.click());
  expect(access.title).toBe("Read only");
  expect(access.getAttribute("aria-pressed")).toBe("false");
  act(() =>
    host.querySelector<HTMLButtonElement>(".reference-chip-remove")!.click(),
  );
  expect(host.querySelector(".reference-chip")).toBeNull();
});
