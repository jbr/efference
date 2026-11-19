// @vitest-environment jsdom
// The hook used to declare a third `document` argument and drop it, so a
// recalled structured turn went out through the legacy text path. This binds
// the regression to the hook rather than to actions.submitTurn directly.

import { act } from "react";
import { createRoot } from "react-dom/client";
import { afterEach, expect, test, vi } from "vitest";
import type { UserContent } from "../api/types.ts";
import { useSession, type SessionActions } from "./useSession.ts";
import { setDevSetting } from "./devSettings.ts";

Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true });

const { submitTurnMock } = vi.hoisted(() => ({
  submitTurnMock: vi.fn(() => Promise.resolve()),
}));

vi.mock("./actions.ts", async (importOriginal) => {
  const original = await importOriginal<typeof import("./actions.ts")>();
  return { ...original, submitTurn: submitTurnMock };
});

const { connectMock, recordMock, debugMock } = vi.hoisted(() => ({
  // The implementation drops its arguments; the test reads them back off
  // `connectMock.mock.calls` instead.
  connectMock: vi.fn((...args: unknown[]): { retry(): void; close(): void } => {
    void args;
    return { retry: () => {}, close: () => {} };
  }),
  recordMock: vi.fn(() => ({ tap: {}, scrub: {}, close: () => {} })),
  debugMock: vi.fn(() => ({ observer: {}, close: () => {} })),
}));

vi.mock("./connect.ts", () => ({ connect: connectMock }));

// The opt-in dev branch of the effect dynamically imports these; the mocks
// keep the test off the recording machinery while exposing what was installed.
vi.mock("./record.ts", () => ({ record: recordMock }));
vi.mock("../ui/streamDebug.ts", () => ({ installStreamDebug: debugMock }));

afterEach(() => {
  vi.clearAllMocks();
  setDevSetting("scrub", false);
  setDevSetting("streamDebug", false);
});

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

test("the scrubber and stream debug install only when their switch is on", async () => {
  window.history.replaceState({}, "", "/sessions/dev-tools-install");
  function Probe() {
    useSession();
    return null;
  }
  const host = document.createElement("div");
  document.body.append(host);
  const root = createRoot(host);
  const settle = () =>
    act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 0));
    });
  try {
    await act(async () => {
      root.render(<Probe />);
      await Promise.resolve();
    });
    // Off by default, even in a dev build: the plain connection, with no
    // frame tap and no stream observer.
    expect(connectMock).toHaveBeenCalledTimes(1);
    expect(connectMock.mock.calls[0][1]).toBeUndefined();
    expect(connectMock.mock.calls[0][3]).toBeUndefined();

    act(() => setDevSetting("scrub", true));
    await settle();
    expect(connectMock.mock.calls.at(-1)![1]).toBeDefined();
    expect(connectMock.mock.calls.at(-1)![3]).toBeUndefined();

    act(() => setDevSetting("streamDebug", true));
    await settle();
    expect(connectMock.mock.calls.at(-1)![1]).toBeDefined();
    expect(connectMock.mock.calls.at(-1)![3]).toBeDefined();
  } finally {
    act(() => root.unmount());
    host.remove();
  }
});
