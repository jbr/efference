// @vitest-environment jsdom
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, expect, test, vi } from "vitest";
import { useNewSessionShortcut } from "./useNewSessionShortcut.ts";

Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true });
let host: HTMLDivElement;
let root: Root;
let installed: boolean;
function Probe({ open }: { open: (() => void) | null }) {
  useNewSessionShortcut(open);
  return <input />;
}
function press(options: KeyboardEventInit = {}) {
  const event = new KeyboardEvent("keydown", {
    key: "n",
    metaKey: true,
    bubbles: true,
    cancelable: true,
    ...options,
  });
  act(() => document.activeElement!.dispatchEvent(event));
  return event;
}
beforeEach(() => {
  installed = true;
  vi.stubGlobal("matchMedia", () => ({ matches: installed }));
  host = document.createElement("div");
  document.body.append(host);
  root = createRoot(host);
});
afterEach(() => {
  act(() => root.unmount());
  host.remove();
  vi.unstubAllGlobals();
});

test("Cmd+N runs the current action from a focused input and cancels native behavior", () => {
  const first = vi.fn();
  const next = vi.fn();
  act(() => root.render(<Probe open={first} />));
  host.querySelector("input")!.focus();
  expect(press().defaultPrevented).toBe(true);
  expect(first).toHaveBeenCalledTimes(1);
  expect(press({ repeat: true }).defaultPrevented).toBe(true);
  expect(first).toHaveBeenCalledTimes(1);
  act(() => root.render(<Probe open={next} />));
  expect(press().defaultPrevented).toBe(true);
  expect(next).toHaveBeenCalledTimes(1);
  expect(first).toHaveBeenCalledTimes(1);
});

test("browser mode, extra modifiers and already-handled events retain their behavior", () => {
  const open = vi.fn();
  act(() => root.render(<Probe open={open} />));
  installed = false;
  expect(press().defaultPrevented).toBe(false);
  installed = true;
  for (const options of [
    { metaKey: false },
    { ctrlKey: true },
    { altKey: true },
    { shiftKey: true },
    { key: "m" },
  ])
    expect(press(options).defaultPrevented).toBe(false);
  const event = new KeyboardEvent("keydown", {
    key: "n",
    metaKey: true,
    cancelable: true,
  });
  event.preventDefault();
  window.dispatchEvent(event);
  expect(open).not.toHaveBeenCalled();
});

test("unavailable actions and unmounted surfaces leave Cmd+N alone", () => {
  const open = vi.fn();
  act(() => root.render(<Probe open={open} />));
  act(() => root.render(<Probe open={null} />));
  expect(press().defaultPrevented).toBe(false);
  act(() => root.render(<Probe open={open} />));
  act(() => root.render(null));
  expect(press().defaultPrevented).toBe(false);
  expect(open).not.toHaveBeenCalled();
});
