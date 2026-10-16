// @vitest-environment jsdom
import { act } from "react";
import { createRoot } from "react-dom/client";
import { expect, test, vi } from "vitest";
import { CompactControl } from "./CompactControl.tsx";
import { compactLoop } from "../api/http.ts";
import type { Verbs } from "../api/types.ts";
vi.mock("../api/http.ts", () => ({
  compactLoop: vi.fn(),
  ApiError: class extends Error {},
}));
Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true });
const verbs = (compact: Verbs["compact"]): Verbs => ({
  turn: { accepted: true, delivery: "now" },
  continue: { accepted: true },
  compact,
});
const defaults = {
  cold: null,
  verbs: verbs({ accepted: true, warm: true }),
  operations: null,
  failure: null,
};

test("one click directly summarizes and supplies warm consent without confirmation", async () => {
  const host = document.createElement("div");
  const root = createRoot(host);
  let finish!: () => void;
  vi.mocked(compactLoop)
    .mockClear()
    .mockImplementation(
      () =>
        new Promise<{ status: string }>((resolve) => {
          finish = () => resolve({ status: "started" });
        }),
    );
  try {
    act(() => root.render(<CompactControl {...defaults} />));
    act(() => host.querySelector<HTMLButtonElement>("button")!.click());
    expect(compactLoop).toHaveBeenCalledExactlyOnceWith(true);
    expect(host.textContent).toBe("Summarizing the history…");
    expect(host.querySelector("button")).toBeNull();
    await act(async () => finish());
  } finally {
    act(() => root.unmount());
  }
});

test("a refusal the route publishes disables summary with its reason, and a cold session summarizes without warm", async () => {
  const host = document.createElement("div");
  const root = createRoot(host);
  vi.mocked(compactLoop).mockClear().mockResolvedValue({ status: "started" });
  try {
    for (const message of [
      "an approval gate is pending — resolve it first",
      "a question is pending — answer it first",
      "the loop is running — compact when it stops",
    ]) {
      act(() =>
        root.render(
          <CompactControl
            {...defaults}
            verbs={verbs({ accepted: false, message })}
          />,
        ),
      );
      expect(host.querySelector<HTMLButtonElement>("button")!.disabled).toBe(
        true,
      );
      expect(host.textContent).toContain(message);
      act(() => host.querySelector<HTMLButtonElement>("button")!.click());
    }
    expect(compactLoop).not.toHaveBeenCalled();
    act(() =>
      root.render(
        <CompactControl
          {...defaults}
          verbs={verbs({ accepted: true, warm: false })}
          cold={{ since: 1, context_tokens: 100 }}
        />,
      ),
    );
    await act(async () =>
      host.querySelector<HTMLButtonElement>("button")!.click(),
    );
    expect(compactLoop).toHaveBeenCalledExactlyOnceWith(false);
  } finally {
    act(() => root.unmount());
  }
});

test("summary request errors remain visible and permit retry", async () => {
  const host = document.createElement("div");
  const root = createRoot(host);
  vi.mocked(compactLoop).mockClear().mockRejectedValue(new Error("offline"));
  try {
    act(() => root.render(<CompactControl {...defaults} />));
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
