// @vitest-environment jsdom
import { act } from "react";
import { createRoot } from "react-dom/client";
import { expect, test, vi } from "vitest";
import { FoldControl } from "./FoldControl.tsx";
import { foldLoop } from "../api/http.ts";
vi.mock("../api/http.ts", () => ({
  foldLoop: vi.fn(),
  ApiError: class extends Error {},
}));
Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true });
const defaults = {
  cold: null,
  fold: null,
  running: false,
  gatePending: false,
  asking: false,
};

test("one click directly summarizes and supplies warm consent without confirmation", async () => {
  const host = document.createElement("div");
  const root = createRoot(host);
  let finish!: () => void;
  vi.mocked(foldLoop)
    .mockClear()
    .mockImplementation(
      () =>
        new Promise<{ status: string }>((resolve) => {
          finish = () => resolve({ status: "started" });
        }),
    );
  try {
    act(() => root.render(<FoldControl {...defaults} />));
    act(() => host.querySelector<HTMLButtonElement>("button")!.click());
    expect(foldLoop).toHaveBeenCalledExactlyOnceWith(true);
    expect(host.textContent).toBe("Summarizing the history…");
    expect(host.querySelector("button")).toBeNull();
    await act(async () => finish());
  } finally {
    act(() => root.unmount());
  }
});

test("pending approvals and questions block summary, while a cold parked loop can summarize", async () => {
  const host = document.createElement("div");
  const root = createRoot(host);
  vi.mocked(foldLoop).mockClear().mockResolvedValue({ status: "started" });
  try {
    for (const blocked of [
      { gatePending: true },
      { asking: true },
      { running: true },
    ]) {
      act(() => root.render(<FoldControl {...defaults} {...blocked} />));
      expect(host.querySelector<HTMLButtonElement>("button")!.disabled).toBe(
        true,
      );
      act(() => host.querySelector<HTMLButtonElement>("button")!.click());
    }
    expect(foldLoop).not.toHaveBeenCalled();
    act(() =>
      root.render(
        <FoldControl
          {...defaults}
          running
          cold={{ since: 1, context_tokens: 100 }}
        />,
      ),
    );
    await act(async () =>
      host.querySelector<HTMLButtonElement>("button")!.click(),
    );
    expect(foldLoop).toHaveBeenCalledExactlyOnceWith(false);
  } finally {
    act(() => root.unmount());
  }
});

test("summary request errors remain visible and permit retry", async () => {
  const host = document.createElement("div");
  const root = createRoot(host);
  vi.mocked(foldLoop).mockClear().mockRejectedValue(new Error("offline"));
  try {
    act(() => root.render(<FoldControl {...defaults} />));
    await act(async () =>
      host.querySelector<HTMLButtonElement>("button")!.click(),
    );
    expect(host.querySelector('[role="alert"]')?.textContent).toContain(
      "offline",
    );
    expect(host.querySelector<HTMLButtonElement>("button")!.disabled).toBe(
      false,
    );
  } finally {
    act(() => root.unmount());
  }
});
