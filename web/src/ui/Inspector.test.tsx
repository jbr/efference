// @vitest-environment jsdom
import { act } from "react";
import { createRoot } from "react-dom/client";
import { afterEach, beforeEach, expect, test, vi } from "vitest";
import { Inspector } from "./Inspector.tsx";
import { fetchProcessOutput, fetchProcessRecord } from "../api/http.ts";
import { messagesAssistant } from "../session/assistant.ts";
import { buildTree, type CallNode } from "../session/tree.ts";
import type { LogEntry, RawResult, WireState } from "../api/types.ts";
vi.mock("../api/http.ts", async (actual) => ({
  ...(await actual<typeof import("../api/http.ts")>()),
  fetchProcessOutput: vi.fn(),
  fetchProcessRecord: vi.fn(),
}));
vi.mock("./Code.tsx", () => ({
  Code: ({ text }: { text: string }) => <pre>{text}</pre>,
}));
Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true });
beforeEach(() =>
  vi.stubGlobal(
    "ResizeObserver",
    class {
      observe() {}
      disconnect() {}
    },
  ),
);
afterEach(() => {
  vi.resetAllMocks();
  vi.unstubAllGlobals();
});

const entry = (seq: number, payload: LogEntry["payload"]): LogEntry => ({
  seq,
  turn: 1,
  at: 0,
  payload,
});

/** A bash call through to its telemetry row, built the way the store folds
 * the log (session/tree.ts). */
function bashCall(
  raw: RawResult,
  wire: WireState,
  background: boolean,
): CallNode {
  const nodes = buildTree([
    entry(1, {
      Assistant: messagesAssistant(
        [],
        [],
        [{ id: "call-env", name: "bash", input: { command: "env" } }],
      ),
    }),
    entry(2, { ToolResult: { call_id: "call-env", raw, wire } }),
    entry(3, {
      Telemetry: {
        Tool: {
          call_id: "call-env",
          tool: "bash",
          duration_ms: 36,
          outcome: "applied",
          result_bytes: raw.content.length,
          bash: { background, handle: "p#4" },
        },
      },
    }),
  ]);
  const batch = nodes.find((n) => n.kind === "batch");
  if (batch?.kind !== "batch") throw new Error("no batch");
  return batch.calls[0];
}

async function inspect(node: CallNode, processId: number | undefined) {
  const host = document.createElement("div");
  const root = createRoot(host);
  await act(async () =>
    root.render(
      <Inspector
        node={node}
        closing={false}
        onSlideOut={() => {}}
        onClose={() => {}}
        onOpenFile={() => {}}
        liveOutput={undefined}
        processId={processId}
        processOutput={undefined}
        processLost={undefined}
      />,
    ),
  );
  return { host, unmount: () => act(() => root.unmount()) };
}

const lines = Array.from({ length: 30 }, (_, i) => `VAR_${i + 1}=x`);

// The regression this pins: a client that watched a foreground command run
// learned its process id from the `processes` topic, and the drawer read the
// id as a handoff — hid the result, and asked the process monitor about a
// process it drops on completion (docs/process-monitor.md).
test("a foreground command the client saw running shows its triaged result", async () => {
  const node = bashCall(
    { content: lines.join("\n") + "\n", is_error: false },
    {
      Triaged: {
        keep: {
          Ranges: [
            [5, 5],
            [20, 21],
          ],
        },
        note: "the two vars that matter",
      },
    },
    false,
  );
  const { host, unmount } = await inspect(node, 4);
  try {
    const text = host.textContent ?? "";
    expect(text).toContain("env · p#4");
    expect(text).toContain("Finished · exit 0 · 36 ms");
    expect(text).toContain("triaged, kept 3 of 30 lines");
    expect(text).toContain("the two vars that matter");
    expect(text).toContain("VAR_5=x");
    expect(text).toContain("VAR_21=x");
    expect(text).not.toContain("VAR_6=x");
    expect(text).toContain("14 lines omitted");
    expect(text).not.toContain("Process state unavailable");
    expect(text).not.toContain("Raw tool result");
    expect(fetchProcessRecord).not.toHaveBeenCalled();
    expect(fetchProcessOutput).not.toHaveBeenCalled();
  } finally {
    unmount();
  }
});

test("a backgrounded command shows the process capture, not its handoff result", async () => {
  vi.mocked(fetchProcessOutput).mockResolvedValue({
    id: 4,
    output: "CAPTURED OUTPUT",
    cursor: 15,
    lost: 0,
    running: false,
  });
  vi.mocked(fetchProcessRecord).mockResolvedValue({
    id: 4,
    pid: 99,
    command: "env",
    spawned_at: 0,
    background: true,
    muted: false,
    running: false,
    exit: { outcome: { code: 0 }, duration_ms: 2500, at: 0 },
  });
  const node = bashCall(
    {
      content: "backgrounded as p#4. its output reaches you…",
      is_error: false,
    },
    "Raw",
    true,
  );
  const { host, unmount } = await inspect(node, 4);
  try {
    const text = host.textContent ?? "";
    expect(fetchProcessOutput).toHaveBeenCalledWith(4);
    expect(host.querySelector(".command-output")?.textContent).toBe(
      "CAPTURED OUTPUT",
    );
    expect(text).toContain("Finished · exit 0 · 2.5s · pid 99");
    expect(text).toContain("Raw tool result");
  } finally {
    unmount();
  }
});
