// @vitest-environment jsdom
import { act } from "react";
import { createRoot } from "react-dom/client";
import { afterEach, expect, test, vi } from "vitest";
import { useHistoricalCall } from "./useHistoricalCall.ts";
import { fetchCallSeq, fetchLogRange } from "../../api/http.ts";
import { messagesAssistant } from "../../session/assistant.ts";
import { buildTree, type CallNode } from "../../session/tree.ts";
import type { LogEntry } from "../../api/entry.ts";
import type { ToolCall } from "../../api/types.ts";
vi.mock("../../api/http.ts", () => ({
  fetchCallSeq: vi.fn(),
  fetchLogRange: vi.fn(),
}));
Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true });
afterEach(() => vi.resetAllMocks());
const issuedCall: ToolCall = {
  id: "old-call",
  name: "bash",
  input: { command: "sleep 10" },
};
const issued: LogEntry = {
  seq: 2,
  turn: 1,
  at: 0,
  payload: { Assistant: messagesAssistant([], [], [issuedCall]) },
};
const current: LogEntry[] = [
  {
    seq: 20,
    turn: 2,
    at: 0,
    payload: { Assistant: messagesAssistant([], [{ Text: "recent" }], []) },
  },
];
test("off-window task call loads inspector-only history using pinned base", async () => {
  vi.mocked(fetchCallSeq).mockResolvedValue({ seq: 2, call: issuedCall });
  vi.mocked(fetchLogRange).mockResolvedValue([issued]);
  const root = createRoot(document.createElement("div"));
  let value: ReturnType<typeof useHistoricalCall> | undefined;
  function Probe() {
    value = useHistoricalCall("old-call", null, current, "/sessions/origin");
    return null;
  }
  try {
    await act(async () => root.render(<Probe />));
    expect(fetchCallSeq).toHaveBeenCalledWith("old-call", "/sessions/origin");
    expect(fetchLogRange).toHaveBeenCalledWith(2, 21, "/sessions/origin");
    expect(value?.node?.callId).toBe("old-call");
    expect(current).toHaveLength(1);
  } finally {
    act(() => root.unmount());
  }
});
test("already loaded call does not fetch historical window", async () => {
  const batch = buildTree([issued]).find((n) => n.kind === "batch");
  const call = batch?.kind === "batch" ? batch.calls[0] : null;
  const root = createRoot(document.createElement("div"));
  function Probe() {
    useHistoricalCall(
      "old-call",
      call as CallNode,
      current,
      "/sessions/origin",
    );
    return null;
  }
  try {
    await act(async () => root.render(<Probe />));
    expect(fetchCallSeq).not.toHaveBeenCalled();
  } finally {
    act(() => root.unmount());
  }
});
