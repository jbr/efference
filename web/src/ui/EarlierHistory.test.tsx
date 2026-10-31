// @vitest-environment jsdom
import { act } from "react";
import { createRoot } from "react-dom/client";
import { afterEach, expect, it, vi } from "vitest";
import { fetchLogBefore } from "../api/http.ts";
import type { LogEntry } from "../api/entry.ts";
import { HistoryLoader } from "../session/history.ts";
import { seed, Store } from "../session/store.ts";
import { EarlierHistory } from "./EarlierHistory.tsx";
import { captureHistoryAnchor } from "./historyAnchor.ts";
const intent = vi.hoisted(() => ({ following: true }));
vi.mock("./followTail.ts", () => ({
  subscribeTail: () => () => {},
  tailFollowing: () => intent.following,
}));
vi.mock("../api/http.ts", () => ({
  fetchLogBefore: vi.fn(),
  fetchCallSeq: vi.fn(),
  fetchLogRange: vi.fn(),
}));
Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true });
afterEach(() => {
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
  vi.resetAllMocks();
  document.body.replaceChildren();
});
const entry = (seq: number): LogEntry => ({
  seq,
  turn: 1,
  at: 0,
  payload: { User: [{ Text: "message" }] },
});
it("compensates a visible node's residual movement, not total document growth", () => {
  const node = document.createElement("div");
  node.dataset.transcriptKey = "stable";
  document.body.append(node);
  let top = -20;
  vi.spyOn(node, "getBoundingClientRect").mockImplementation(
    () => ({ top, bottom: top + 200 }) as DOMRect,
  );
  const scroll = vi.spyOn(window, "scrollBy").mockImplementation(() => {});
  const restore = captureHistoryAnchor()!;
  top = 300;
  restore();
  expect(scroll).toHaveBeenCalledWith({ top: 320, behavior: "instant" });
  scroll.mockClear();
  top = -20;
  restore();
  expect(scroll).not.toHaveBeenCalled();
  node.remove();
  top = 200;
  restore();
  expect(scroll).not.toHaveBeenCalled();
});
async function mounted(following: boolean, height: number) {
  intent.following = following;
  Object.defineProperty(document.documentElement, "scrollHeight", {
    configurable: true,
    value: height,
  });
  Object.defineProperty(window, "innerHeight", {
    configurable: true,
    value: 800,
  });
  let callback: IntersectionObserverCallback | undefined;
  const disconnect = vi.fn();
  vi.stubGlobal(
    "IntersectionObserver",
    class {
      constructor(cb: IntersectionObserverCallback) {
        callback = cb;
      }
      observe() {}
      disconnect = disconnect;
    },
  );
  const store = new Store((flush) => flush());
  store.update((s) => seed(s, [entry(100)]));
  const history = new HistoryLoader(store, "/sessions/source");
  const host = document.createElement("div");
  document.body.append(host);
  const root = createRoot(host);
  await act(async () => {
    root.render(
      <EarlierHistory history={history} entries={store.snapshot().entries} />,
    );
  });
  return {
    host,
    root,
    disconnect,
    intersect: () =>
      callback?.(
        [{ isIntersecting: true } as IntersectionObserverEntry],
        {} as IntersectionObserver,
      ),
  };
}
it("does not page while following a scrollable tail", async () => {
  const view = await mounted(true, 2000);
  try {
    await act(async () => view.intersect());
    expect(fetchLogBefore).not.toHaveBeenCalled();
  } finally {
    act(() => view.root.unmount());
  }
});
it.each([
  [false, 2000],
  [true, 500],
])(
  "pages near the top with following=%s and height=%s",
  async (following, height) => {
    vi.mocked(fetchLogBefore).mockResolvedValue([]);
    const view = await mounted(following, height);
    try {
      await act(async () => view.intersect());
      expect(fetchLogBefore).toHaveBeenCalledOnce();
      expect(view.host.textContent).toBe("Beginning of session");
    } finally {
      act(() => view.root.unmount());
    }
  },
);
it("does not automatically spin on a failed page and exposes a working retry", async () => {
  vi.mocked(fetchLogBefore)
    .mockRejectedValueOnce(new Error("offline"))
    .mockResolvedValueOnce([]);
  const view = await mounted(false, 2000);
  try {
    await act(async () => view.intersect());
    expect(view.host.textContent).toContain("Could not load earlier entries.");
    expect(view.disconnect).toHaveBeenCalled();
    await act(async () =>
      view.host.querySelector<HTMLButtonElement>("button")!.click(),
    );
    expect(fetchLogBefore).toHaveBeenCalledTimes(2);
    expect(view.host.textContent).toBe("Beginning of session");
  } finally {
    act(() => view.root.unmount());
  }
});
