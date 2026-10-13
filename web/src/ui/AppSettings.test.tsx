// @vitest-environment jsdom
import { act } from "react";
import { createRoot } from "react-dom/client";
import { afterEach, expect, test, vi } from "vitest";
import { AppSettings } from "./AppSettings.tsx";
import { settingsHref } from "./settingsHref.ts";
import { navigate } from "./route.ts";
Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true });
afterEach(() => {
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
});

test("settings URLs retain the underlying page and other query state", () => {
  expect(settingsHref("/sessions/one/", "?other=1", true)).toBe(
    "/sessions/one/?other=1&settings=",
  );
  expect(settingsHref("/", "?settings=&other=1", false)).toBe("/?other=1");
});

test("settings opens and closes without scrolling or remounting its underlying page", () => {
  history.replaceState(null, "", "/sessions/settings-test/");
  const values = new Map<string, string>();
  vi.stubGlobal("localStorage", {
    getItem: (key: string) => values.get(key) ?? null,
    setItem: (key: string, value: string) => values.set(key, value),
    removeItem: (key: string) => values.delete(key),
  });
  const show = vi.fn();
  const close = vi.fn();
  Object.defineProperty(HTMLDialogElement.prototype, "showModal", {
    configurable: true,
    value: show,
  });
  Object.defineProperty(HTMLDialogElement.prototype, "close", {
    configurable: true,
    value: close,
  });
  const scroll = vi.spyOn(window, "scrollTo").mockImplementation(() => {});
  const host = document.createElement("div");
  const root = createRoot(host);
  try {
    act(() => root.render(<AppSettings />));
    expect(host.querySelector("dialog")).toBeNull();
    act(() => navigate("/sessions/settings-test/?settings="));
    expect(show).toHaveBeenCalledOnce();
    expect(host.querySelector("h1")?.textContent).toBe("App settings");
    expect(scroll).not.toHaveBeenCalled();
    act(() => host.querySelector<HTMLButtonElement>("button")!.click());
    expect(location.pathname).toBe("/sessions/settings-test/");
    expect(location.search).toBe("");
    expect(host.querySelector("dialog")).toBeNull();
    expect(close).toHaveBeenCalledOnce();
  } finally {
    act(() => root.unmount());
    delete (HTMLDialogElement.prototype as unknown as { showModal?: unknown })
      .showModal;
    delete (HTMLDialogElement.prototype as unknown as { close?: unknown })
      .close;
  }
});
