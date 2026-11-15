// @vitest-environment jsdom
// The hook used to declare a third `document` argument and drop it, so a
// recalled structured turn went out through the legacy text path. This binds
// the regression to the hook rather than to actions.submitTurn directly.

import { act } from "react";
import { createRoot } from "react-dom/client";
import { afterEach, expect, test, vi } from "vitest";
import type { UserContent } from "../api/types.ts";
import { useSession, type SessionActions } from "./useSession.ts";

Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true });

const { submitTurnMock } = vi.hoisted(() => ({
  submitTurnMock: vi.fn(() => Promise.resolve()),
}));

vi.mock("./actions.ts", async (importOriginal) => {
  const original = await importOriginal<typeof import("./actions.ts")>();
  return { ...original, submitTurn: submitTurnMock };
});

vi.mock("./connect.ts", () => ({
  connect: () => ({ retry: () => {}, close: () => {} }),
}));

// The dev branch of the effect dynamically imports these; keep the test off
// the recording machinery entirely.
vi.mock("./record.ts", () => ({
  record: () => ({ tap: () => {}, scrub: () => {}, close: () => {} }),
}));
vi.mock("../ui/streamDebug.ts", () => ({
  installStreamDebug: () => ({ observer: null, close: () => {} }),
}));

afterEach(() => vi.clearAllMocks());

test("useSession forwards a structured turn's document", async () => {
  window.history.replaceState({}, "", "/sessions/use-session-document");
  let actions: SessionActions | null = null;
  function Probe() {
    actions = useSession().actions;
    return null;
  }
  const host = document.createElement("div");
  document.body.append(host);
  const root = createRoot(host);
  try {
    await act(async () => {
      root.render(<Probe />);
      await Promise.resolve();
    });
    const document: UserContent = {
      content: [{ Text: "look " }, { Reference: "r1" }],
      references: {
        r1: {
          label: "main.tsx",
          target: {
            type: "filesystem",
            original: "./main.tsx",
            path: "/w/main.tsx",
            target: "file",
            access: { read: true, write: false },
          },
        },
      },
    };
    await act(async () => {
      await actions!.submitTurn("look ./main.tsx", [], document);
    });
    expect(submitTurnMock).toHaveBeenCalledTimes(1);
    const call = submitTurnMock.mock.calls[0] as unknown as [
      unknown,
      string,
      unknown,
      UserContent,
    ];
    expect(call[1]).toBe("look ./main.tsx");
    expect(call[2]).toEqual([]);
    expect(call[3]).toEqual(document);
  } finally {
    act(() => root.unmount());
    host.remove();
  }
});
