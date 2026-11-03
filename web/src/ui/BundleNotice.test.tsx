// @vitest-environment jsdom
import { act, type ReactNode } from "react";
import { createRoot } from "react-dom/client";
import { expect, test, vi } from "vitest";
import { BundleNotice, BundleNoticeStrip } from "./BundleNotice.tsx";

Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true });

/** The verdict the notice reads, as the module under test would answer. */
const verdict = vi.hoisted(() => ({ value: "unknown" as string }));
vi.mock("../api/bundle.ts", () => ({
  bundleState: () => verdict.value,
  subscribeBundle: () => () => {},
}));

function render(ui: ReactNode) {
  const host = document.createElement("div");
  const root = createRoot(host);
  act(() => root.render(ui));
  return host;
}

test("nothing is said while the page's build is the host's", () => {
  verdict.value = "current";
  expect(render(<BundleNotice />).innerHTML).toBe("");
  verdict.value = "unknown";
  expect(render(<BundleNotice />).innerHTML).toBe("");
});

test("a page running an older build says so and offers the reload", () => {
  verdict.value = "stale";
  const host = render(<BundleNotice />);
  const notice = host.querySelector(".bundle-notice");
  expect(notice?.getAttribute("role")).toBe("status");
  expect(notice?.textContent).toContain("efference was updated");
  const reload = host.querySelector<HTMLButtonElement>(".bundle-reload");
  expect(reload?.textContent).toBe("Reload");
});

test("the reload is the person's action, never the page's", () => {
  // Nothing in the component reloads on mount or on a change: a reload costs
  // the page's place and focus, and an installed app has no chrome to take it
  // back with, so the only path is the button.
  const onReload = vi.fn();
  const host = render(<BundleNoticeStrip onReload={onReload} />);
  expect(onReload).not.toHaveBeenCalled();
  act(() => host.querySelector<HTMLButtonElement>(".bundle-reload")!.click());
  expect(onReload).toHaveBeenCalledOnce();
});
