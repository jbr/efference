// @vitest-environment jsdom
import { afterEach, expect, test, vi } from "vitest";
import { installStreamDebug, RENDER_EVENT } from "./streamDebug.ts";
import { Store } from "../session/store.ts";
import type { ScrubHandle } from "../session/record.ts";

const scrub: ScrubHandle = {
  cursor: 0,
  length: 20_000,
  following: false,
  expired: true,
  evicted: 42,
  hold() {},
  resume() {},
  step() {},
  seek() {},
  restore: async () => {},
  tapes: async () => [],
};
afterEach(() => vi.useRealTimers());

test("overlay separates raw reads, delivered frames, folded state and rendered state", () => {
  vi.useFakeTimers();
  const store = new Store();
  const before = store.snapshot();
  const { observer, close } = installStreamDebug(
    store,
    scrub,
    "/sessions/test",
  );
  try {
    observer.onAttempt?.(10);
    observer.onConnect?.();
    observer.onActivity?.(123);
    observer.onFrame?.({
      class: "output",
      kind: "tool_call_input",
      data: { index: 0, fragment: "x" },
    });
    document.dispatchEvent(
      new CustomEvent(RENDER_EVENT, {
        detail: { base: "/sessions/other", seq: 99 },
      }),
    );
    document.dispatchEvent(
      new CustomEvent(RENDER_EVENT, {
        detail: { base: "/sessions/test", seq: 9 },
      }),
    );
    observer.onDisconnect?.(new Error("stream ended"));
    vi.advanceTimersByTime(2000);
    const text = document.querySelector("[data-stream-debug]")!.textContent;
    expect(text).toContain("requests 1 / connects 1 · since 10");
    expect(text).toContain("bytes 123 · last read 2s");
    expect(text).toContain("frames 1 (log 0, out 1) · last 2s");
    expect(text).toContain("store seq —");
    expect(text).toContain("render seq 9 · last render 2s");
    expect(text).toContain("tape held 0/20000 · rolled 42 · EXPIRED");
    expect(text).toContain("last error stream ended");
    expect(store.snapshot()).toBe(before);
  } finally {
    close();
  }
  expect(document.querySelector("[data-stream-debug]")).toBeNull();
  expect(vi.getTimerCount()).toBe(0);
});
