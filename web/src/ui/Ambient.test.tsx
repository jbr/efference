// @vitest-environment jsdom
import { act } from "react";
import { createRoot } from "react-dom/client";
import { afterEach, expect, test, vi } from "vitest";
import { Ambient } from "./Ambient.tsx";
import { ambientText } from "./ambientText.ts";

Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true });
afterEach(() => {
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
});

test("removes CSI colors and OSC hyperlinks, preserving text and line breaks", () => {
  expect(
    ambientText(
      "\u001b[1;32m✓ passed\u001b[0m\n\u001b]8;;https://example.com\u001b\\link\u001b]8;;\u001b\\",
    ),
  ).toBe("✓ passed\nlink");
  expect(ambientText("\u009b31merror\u009b0m\u001b]0;title\u0007")).toBe(
    "error",
  );
  expect(ambientText("plain [32m text")).toBe("plain [32m text");
});

function mount(overflow: boolean) {
  vi.stubGlobal(
    "ResizeObserver",
    class {
      observe() {}
      disconnect() {}
    },
  );
  vi.spyOn(HTMLElement.prototype, "scrollHeight", "get").mockReturnValue(
    overflow ? 100 : 30,
  );
  vi.spyOn(HTMLElement.prototype, "clientHeight", "get").mockReturnValue(30);
  const host = document.createElement("div");
  const root = createRoot(host);
  act(() =>
    root.render(
      <Ambient
        source="test-watch"
        body={"\u001b[32mpassed\u001b[0m\nsecond line"}
      />,
    ),
  );
  return { host, unmount: () => act(() => root.unmount()) };
}

test("short notices have no disclosure or terminal escapes", () => {
  const { host, unmount } = mount(false);
  expect(host.querySelector("button")).toBeNull();
  expect(host.textContent).toBe("test-watch — passed\nsecond line");
  unmount();
});

test("overflowing notices expand and collapse without losing output", () => {
  const { host, unmount } = mount(true);
  const button = host.querySelector("button")!;
  expect(button.textContent).toBe("Show more");
  expect(button.getAttribute("aria-expanded")).toBe("false");
  act(() => button.click());
  expect(button.textContent).toBe("Show less");
  expect(button.getAttribute("aria-expanded")).toBe("true");
  expect(host.querySelector(".ambient-preview.expanded")).not.toBeNull();
  act(() => button.click());
  expect(host.querySelector(".ambient-preview.expanded")).toBeNull();
  expect(host.textContent).toContain("second line");
  unmount();
});
