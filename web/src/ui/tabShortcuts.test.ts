// @vitest-environment jsdom
import { afterEach, expect, test, vi } from "vitest";
import { adjacentTab, standaloneApp, tabDirection } from "./tabShortcuts.ts";

afterEach(() => vi.unstubAllGlobals());

const key = (code: string, modifiers: KeyboardEventInit = {}) =>
  new KeyboardEvent("keydown", { code, ...modifiers });

test("platform tab bindings use physical codes, including shifted brackets", () => {
  for (const [event, direction] of [
    [key("BracketLeft", { key: "{", metaKey: true, shiftKey: true }), -1],
    [key("BracketRight", { key: "}", metaKey: true, shiftKey: true }), 1],
    [key("Tab", { ctrlKey: true }), 1],
    [key("Tab", { ctrlKey: true, shiftKey: true }), -1],
    [key("PageUp", { ctrlKey: true }), -1],
    [key("PageDown", { ctrlKey: true }), 1],
  ] as const) {
    expect(tabDirection(event, true)).toBe(direction);
    expect(tabDirection(event, false)).toBeNull();
  }
});

test("ordinary typing and unrelated modifier combinations are ignored", () => {
  for (const event of [
    key("Tab"),
    key("BracketRight"),
    key("BracketRight", { metaKey: true }),
    key("Tab", { ctrlKey: true, altKey: true }),
    key("Tab", { ctrlKey: true, metaKey: true }),
    key("PageDown", { ctrlKey: true, shiftKey: true }),
  ])
    expect(tabDirection(event, true)).toBeNull();
});

test("standalone display mode or iOS standalone enables app shortcuts", () => {
  const matchMedia = vi.fn(() => ({ matches: false }));
  vi.stubGlobal("matchMedia", matchMedia);
  vi.stubGlobal("navigator", { standalone: false });
  expect(standaloneApp()).toBe(false);
  expect(matchMedia).toHaveBeenCalledWith("(display-mode: standalone)");
  matchMedia.mockReturnValue({ matches: true });
  expect(standaloneApp()).toBe(true);
  matchMedia.mockReturnValue({ matches: false });
  vi.stubGlobal("navigator", { standalone: true });
  expect(standaloneApp()).toBe(true);
});

test("cycling follows positions with wrap-around and ignores missing or single tabs", () => {
  const tabs = ["gamma", "alpha", "beta"];
  expect(adjacentTab(tabs, "alpha", 1)).toBe("beta");
  expect(adjacentTab(tabs, "alpha", -1)).toBe("gamma");
  expect(adjacentTab(tabs, "beta", 1)).toBe("gamma");
  expect(adjacentTab(tabs, "gamma", -1)).toBe("beta");
  expect(adjacentTab(tabs, "missing", 1)).toBeNull();
  expect(adjacentTab(["alpha"], "alpha", 1)).toBeNull();
  expect(adjacentTab([], "alpha", -1)).toBeNull();
});
