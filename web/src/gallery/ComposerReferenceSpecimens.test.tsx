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

test("Tab accepts the highlighted directory as a reference", () => {
  const { host, key } = mount();
  expect(key("Tab").defaultPrevented).toBe(true);
  expect(host.querySelector(".reference-chip-label")?.textContent).toBe("src/");
  expect(host.querySelector('[role="listbox"]')).toBeNull();
});

test("Enter accepts the highlighted directory without sending", () => {
  const { host, key } = mount();
  expect(key("Enter").defaultPrevented).toBe(true);
  expect(host.querySelector(".reference-chip-label")?.textContent).toBe("src/");
  expect(host.querySelector('[role="listbox"]')).toBeNull();
  expect(host.querySelector('[role="status"]')?.textContent).toContain(
    "Nothing was sent",
  );
});

test("arrows change the active descendant and Escape leaves the draft alone", () => {
  const { host, input, key } = mount();
  const before = input.getAttribute("aria-activedescendant");
  key("ArrowDown");
  expect(input.getAttribute("aria-activedescendant")).not.toBe(before);
  expect(host.querySelector('[aria-selected="true"]')?.textContent).toContain(
    "public/",
  );
  key("Escape");
  expect(input.value).toBe("Please check ./web/");
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
  const access = host.querySelector<HTMLButtonElement>(
    ".reference-chip-access",
  )!;
  expect(access.getAttribute("aria-pressed")).toBe("false");
  act(() => access.click());
  expect(access.getAttribute("aria-pressed")).toBe("true");
  act(() =>
    host.querySelector<HTMLButtonElement>(".reference-chip-remove")!.click(),
  );
  expect(host.querySelector(".reference-chip")).toBeNull();
});
