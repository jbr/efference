// @vitest-environment jsdom
import { act, createRef } from "react";
import { createRoot, type Root } from "react-dom/client";
import { renderToStaticMarkup } from "react-dom/server";
import { afterEach, beforeEach, expect, test, vi } from "vitest";
import { AppHeader } from "./AppHeader.tsx";

let host: HTMLDivElement;
let root: Root;

beforeEach(() => {
  Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true });
  host = document.createElement("div");
  document.body.append(host);
  root = createRoot(host);
});

afterEach(async () => {
  await act(async () => root.unmount());
  host.remove();
});

test("the default wordmark links home and retains page controls", () => {
  const markup = renderToStaticMarkup(
    <AppHeader>
      <button>New session</button>
    </AppHeader>,
  );
  expect(markup).toContain('href="/"');
  expect(markup).toContain('class="wordmark"');
  expect(markup).toContain("New session");
});

test("a page without a list retains a non-link wordmark", () => {
  const markup = renderToStaticMarkup(
    <AppHeader homeHref={null}>Controls</AppHeader>,
  );
  expect(markup).toContain('<span class="wordmark">efference</span>');
  expect(markup).not.toContain("href=");
});

test("the caller's ref measures the actual header", async () => {
  const ref = createRef<HTMLElement>();
  await act(async () => root.render(<AppHeader ref={ref}>Controls</AppHeader>));
  expect(ref.current).toBe(host.querySelector("header"));
});

test("ordinary home clicks use the flow callback without native navigation", async () => {
  const onHome = vi.fn();
  await act(async () =>
    root.render(
      <AppHeader homeHref="#home" onHome={onHome}>
        Controls
      </AppHeader>,
    ),
  );
  const event = new MouseEvent("click", { bubbles: true, cancelable: true });
  await act(async () => host.querySelector("a")!.dispatchEvent(event));
  expect(onHome).toHaveBeenCalledOnce();
  expect(event.defaultPrevented).toBe(true);
});

test.each([
  { metaKey: true },
  { ctrlKey: true },
  { shiftKey: true },
  { altKey: true },
  { button: 1 },
])(
  "modified home click %j retains native browser behavior",
  async (modifier) => {
    const onHome = vi.fn();
    await act(async () =>
      root.render(
        <AppHeader homeHref="#home" onHome={onHome}>
          Controls
        </AppHeader>,
      ),
    );
    const event = new MouseEvent("click", {
      bubbles: true,
      cancelable: true,
      ...modifier,
    });
    await act(async () => host.querySelector("a")!.dispatchEvent(event));
    expect(onHome).not.toHaveBeenCalled();
    expect(event.defaultPrevented).toBe(false);
  },
);
