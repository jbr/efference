// @vitest-environment jsdom
import { act } from "react";
import { createRoot } from "react-dom/client";
import { expect, test, vi } from "vitest";
import { ContextPopup } from "./ContextPopup.tsx";

Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true });

test("context details open on delayed mouse hover, bridge to actions, and support focus and tap", () => {
  vi.useFakeTimers();
  const host = document.createElement("div");
  document.body.append(host);
  const root = createRoot(host);
  const pointer = (node: Element, type: string, pointerType = "mouse") => {
    const event = new Event(type, { bubbles: true });
    Object.defineProperty(event, "pointerType", { value: pointerType });
    act(() => node.dispatchEvent(event));
  };
  const advance = (ms: number) => act(() => vi.advanceTimersByTime(ms));
  try {
    act(() =>
      root.render(
        <ContextPopup
          context={{ used: 50_000, limit: 200_000 }}
          summary={{
            cold: null,
            compaction: null,
            running: false,
            gatePending: false,
            asking: false,
          }}
        />,
      ),
    );
    const trigger = host.querySelector("button")!;
    const position = host.querySelector<HTMLElement>(".hover-popup-position")!;
    const summary = host.querySelector<HTMLButtonElement>(
      ".compact-control button",
    )!;
    expect(position.hidden).toBe(true);
    pointer(trigger, "pointerover");
    advance(179);
    expect(position.hidden).toBe(true);
    advance(1);
    expect(position.hidden).toBe(false);
    pointer(trigger, "pointerout");
    advance(60);
    pointer(position, "pointerover");
    advance(180);
    expect(position.hidden).toBe(false);
    pointer(position, "pointerout");
    advance(120);
    expect(position.hidden).toBe(true);
    act(() =>
      document.dispatchEvent(
        new KeyboardEvent("keydown", { key: "Tab", bubbles: true }),
      ),
    );
    act(() => trigger.focus());
    expect(position.hidden).toBe(false);
    act(() => summary.focus());
    advance(120);
    expect(position.hidden).toBe(false);
    act(() =>
      summary.dispatchEvent(
        new KeyboardEvent("keydown", { key: "Escape", bubbles: true }),
      ),
    );
    expect(position.hidden).toBe(true);
    pointer(trigger, "pointerdown");
    act(() => trigger.focus());
    act(() =>
      trigger.dispatchEvent(
        new MouseEvent("click", { bubbles: true, detail: 1 }),
      ),
    );
    expect(position.hidden).toBe(true);
    pointer(trigger, "pointerover");
    advance(180);
    expect(position.hidden).toBe(false);
    pointer(trigger, "pointerout");
    advance(120);
    expect(position.hidden).toBe(true);
    pointer(trigger, "pointerdown", "touch");
    pointer(trigger, "pointerup", "touch");
    expect(position.hidden).toBe(false);
    pointer(document.body, "pointerdown", "touch");
    expect(position.hidden).toBe(true);
    pointer(trigger, "pointerover", "touch");
    advance(180);
    expect(position.hidden).toBe(true);
  } finally {
    act(() => root.unmount());
    host.remove();
    vi.useRealTimers();
  }
});
