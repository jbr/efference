import { describe, expect, it } from "vitest";
import type { Frame, LogEntry, RequestStage } from "../api/types.ts";
import { messagesAssistant } from "./assistant.ts";
import { applyFrame, emptySession, seed, type Session } from "./store.ts";

const log = (seq: number, payload: LogEntry["payload"]): Frame => ({
  class: "log",
  kind: "entry",
  seq,
  data: { seq, turn: 1, at: 0, payload },
});
const request = (
  seq: number,
  origin: "compaction" | "triage" | "consolidation",
  stage: RequestStage["stage"],
  id = 1,
) => log(seq, { Telemetry: { RequestStage: { request: id, origin, stage } } });
const delta = (
  kind:
    | "consolidation_delta"
    | "consolidation_thinking_delta"
    | "compaction_delta"
    | "compaction_thinking_delta"
    | "triage_delta"
    | "triage_thinking_delta",
  text: string,
  callIds?: string[],
): Frame => ({
  class: "output",
  kind,
  data: callIds === undefined ? { text } : { text, call_ids: callIds },
});
const fold = (...frames: Frame[]) => frames.reduce(applyFrame, emptySession);
const triage = (for_result: number | null): LogEntry["payload"] => ({
  Triage: {
    for_result,
    origin: "Model",
    raw_keep: "all",
    inbox: [],
    surfaced: [],
    retries: [],
  },
});
const result = (call_id: string): LogEntry["payload"] => ({
  ToolResult: {
    call_id,
    raw: { content: "", is_error: false },
    wire: "Pending",
  },
});
const ops = (
  requests: {
    request: number;
    origin: "compaction" | "triage" | "consolidation";
    stage: "sent" | "streaming" | "retrying";
  }[],
): Frame => ({
  class: "state",
  kind: "current_operations",
  data: { requests, tools: [], compaction: null },
});

function expectEmpty(s: Session) {
  expect(s.streaming.compaction).toBeNull();
  expect(s.streaming.triage).toEqual([]);
}

describe("side request streams", () => {
  it("accumulates independent text/thinking, including canonicalized triage batches and idle triage", () => {
    const s = fold(
      delta("compaction_delta", "sum"),
      delta("compaction_delta", "mary"),
      delta("compaction_thinking_delta", "think"),
      delta("triage_delta", "first", ["b", "a"]),
      delta("triage_thinking_delta", "reason", ["a", "b"]),
      delta("triage_delta", "second", ["c"]),
      delta("triage_delta", "idle", []),
    );
    expect(s.streaming.compaction).toEqual({
      text: "summary",
      thinking: "think",
    });
    expect(s.streaming.triage).toEqual([
      { callIds: ["a", "b"], text: "first", thinking: "reason" },
      { callIds: ["c"], text: "second", thinking: "" },
      { callIds: [], text: "idle", thinking: "" },
    ]);
  });

  it("keeps side streams across unrelated main boundaries, even during Assistant draining", () => {
    const s = fold(
      delta("compaction_delta", "a"),
      delta("triage_delta", "b", ["x"]),
      log(1, { Assistant: messagesAssistant([], [{ Text: "done" }], []) }),
      delta("compaction_thinking_delta", "c"),
      delta("triage_thinking_delta", "d", ["x"]),
      {
        class: "output",
        kind: "assistant_delta",
        data: { index: 0, text: "late" },
      },
      log(2, result("unrelated")),
      log(3, { User: [{ Text: "next" }] }),
    );
    expect(s.streaming.blocks).toEqual([]);
    expect(s.streaming.compaction).toEqual({ text: "a", thinking: "c" });
    expect(s.streaming.triage).toEqual([
      { callIds: ["x"], text: "b", thinking: "d" },
    ]);
  });

  it("settles compaction on its Summary, not a consolidation, and drops late output across unrelated logs", () => {
    let s = fold(
      delta("compaction_delta", "a"),
      log(1, { Summary: { from: 0, upto: 0, text: "consolidated" } }),
    );
    expect(s.streaming.compaction?.text).toBe("a");
    s = applyFrame(
      s,
      log(2, {
        Summary: {
          from: 0,
          upto: 0,
          text: "summary",
          made_by: { compaction: {} },
        },
      }),
    );
    s = applyFrame(s, log(3, { User: [{ Text: "next" }] }));
    s = applyFrame(s, delta("compaction_delta", "late"));
    expect(s.streaming.compaction).toBeNull();
    s = applyFrame(s, request(4, "compaction", "sent", 2));
    s = applyFrame(s, delta("compaction_delta", "fresh"));
    expect(s.streaming.compaction?.text).toBe("fresh");
  });

  it("settles matching triage batches by result seq and suppresses late batches without clearing other calls or idle", () => {
    let s = fold(
      log(1, result("a")),
      log(2, result("b")),
      delta("triage_delta", "batch", ["a", "b"]),
      delta("triage_delta", "other", ["c"]),
      delta("triage_delta", "idle", []),
      log(3, triage(1)),
      delta("triage_delta", "late", ["b", "a"]),
    );
    expect(s.streaming.triage.map((tail) => tail.text)).toEqual([
      "other",
      "idle",
    ]);
    s = applyFrame(
      s,
      log(4, { ResultVerdict: { for_result: 2, wire: "Raw" } }),
    );
    s = applyFrame(s, delta("triage_delta", "late b", ["b"]));
    expect(s.streaming.triage.map((tail) => tail.text)).toEqual([
      "other",
      "idle",
    ]);
    s = applyFrame(s, log(5, triage(null)));
    s = applyFrame(s, delta("triage_thinking_delta", "late idle", []));
    expect(s.streaming.triage.map((tail) => tail.text)).toEqual(["other"]);
  });

  it("retains successful tails, clears failed requests, and fences queued deltas until a new request", () => {
    for (const origin of ["compaction", "triage"] as const) {
      for (const terminal of [
        "ended",
        { failed: { reason: "interrupted" } },
      ] as const) {
        const kind =
          origin === "compaction" ? "compaction_delta" : "triage_delta";
        const ids = origin === "triage" ? ["a"] : undefined;
        let s = fold(
          request(1, origin, "sent"),
          delta(kind, "old", ids),
          request(2, origin, terminal),
          delta(kind, "late", ids),
        );
        if (terminal === "ended") {
          expect(
            origin === "compaction"
              ? s.streaming.compaction?.text
              : s.streaming.triage[0]?.text,
          ).toBe("old");
        } else {
          expectEmpty(s);
        }
        s = applyFrame(s, request(3, origin, "sent", 2));
        s = applyFrame(s, delta(kind, "new", ids));
        expect(
          origin === "compaction"
            ? s.streaming.compaction?.text
            : s.streaming.triage[0]?.text,
        ).toBe("new");
      }
    }
  });

  it("resets retries and reopens on their first byte without mixing attempts", () => {
    const s = fold(
      request(1, "triage", "sent"),
      delta("triage_delta", "old", []),
      request(2, "triage", { attempt_failed: { attempt: 1, status: 503 } }),
      delta("triage_delta", "late", []),
      request(3, "triage", "first_byte"),
      delta("triage_delta", "new", []),
    );
    expect(s.streaming.triage).toEqual([
      { callIds: [], text: "new", thinking: "" },
    ]);
  });

  it("uses current_operations for mid-request seeds and completion, without reopening a settled request", () => {
    const active = ops([{ request: 3, origin: "triage", stage: "streaming" }]);
    let s = fold(
      active,
      delta("triage_delta", "idle", []),
      log(1, triage(null)),
      active,
      delta("triage_delta", "late", []),
    );
    expectEmpty(s);
    s = applyFrame(
      s,
      ops([{ request: 4, origin: "triage", stage: "streaming" }]),
    );
    s = applyFrame(s, delta("triage_delta", "new", []));
    expect(s.streaming.triage[0]?.text).toBe("new");
    s = applyFrame(s, ops([]));
    s = applyFrame(s, delta("triage_delta", "late", []));
    expect(s.streaming.triage[0]?.text).toBe("new");
  });

  it("clears compaction failure state and telemetry; Resumed closes both streams", () => {
    for (const failure of [
      {
        class: "state",
        kind: "compaction_failure",
        data: { error: "failed" },
      } as Frame,
      log(1, { Telemetry: { CompactionFailed: { error: "failed" } } }),
    ]) {
      const s = fold(
        delta("compaction_delta", "old"),
        failure,
        delta("compaction_delta", "late"),
      );
      expect(s.streaming.compaction).toBeNull();
    }
  });

  it("closes both side requests on host resume", () => {
    const s = fold(
      delta("compaction_delta", "old"),
      delta("triage_delta", "old", []),
      log(1, { Resumed: { gap_ms: 100 } }),
      delta("compaction_delta", "late"),
      delta("triage_delta", "late", []),
    );
    expectEmpty(s);
  });

  it("settles side buffers during a log reseed too", () => {
    const s = fold(delta("triage_delta", "idle", []));
    const frame = log(1, triage(null));
    if (frame.class !== "log") throw new Error("expected log");
    const next = seed(s, [frame.data]);
    expect(
      applyFrame(next, delta("triage_delta", "late", [])).streaming.triage,
    ).toEqual([]);
  });

  it("caps each text and thinking buffer independently", () => {
    const large = "x".repeat(256 * 1024);
    const s = fold(
      delta("compaction_delta", large),
      delta("compaction_delta", "z"),
      delta("compaction_thinking_delta", "think"),
      delta("triage_thinking_delta", large, []),
      delta("triage_thinking_delta", "y", []),
    );
    expect(s.streaming.compaction?.text).toBe(large.slice(1) + "z");
    expect(s.streaming.compaction?.thinking).toBe("think");
    expect(s.streaming.triage[0]?.thinking).toBe(large.slice(1) + "y");
  });
});

const checking = (seq: number, outcome = "checking", from = 2, upto = 8) =>
  log(seq, {
    Telemetry: {
      Consolidation: {
        from,
        upto,
        context: 10,
        tool_bytes: 65536,
        outcome,
        account_bytes: 0,
      },
    },
  });

describe("consolidation stream", () => {
  it("anchors checking before sent, retains successful text and fences late deltas", () => {
    let s = fold(
      checking(1),
      ops([]),
      request(2, "consolidation", "sent"),
      delta("consolidation_delta", "account"),
      delta("consolidation_thinking_delta", "thought"),
      log(3, { Assistant: { output: { Messages: [{ Text: "main" }] } } }),
    );
    expect(s.streaming.consolidation).toEqual({
      from: 2,
      upto: 8,
      text: "account",
      thinking: "thought",
    });
    s = applyFrame(s, request(4, "consolidation", "ended"));
    s = applyFrame(s, delta("consolidation_delta", "late"));
    expect(s.streaming.consolidation?.text).toBe("account");
    s = applyFrame(s, checking(5, "adopted"));
    s = applyFrame(s, delta("consolidation_delta", "later"));
    expect(s.streaming.consolidation).toBeNull();
  });

  it("settles only matching summaries and seeds span from log history", () => {
    const initial = fold(checking(1));
    let s = seed(emptySession, initial.entries);
    s = applyFrame(s, delta("consolidation_delta", "draft"));
    s = applyFrame(
      s,
      log(2, {
        Summary: {
          from: 1,
          upto: 7,
          text: "other",
        },
      }),
    );
    expect(s.streaming.consolidation?.text).toBe("draft");
    s = applyFrame(
      s,
      log(3, {
        Summary: {
          from: 2,
          upto: 8,
          text: "adopted",
        },
      }),
    );
    expect(s.streaming.consolidation).toBeNull();
    expect(seed(emptySession, s.entries).streaming.consolidation).toBeNull();
  });

  it("preserves span across retries and clears on new checking, failure or resume", () => {
    let s = fold(
      checking(1),
      request(2, "consolidation", "sent"),
      delta("consolidation_delta", "old"),
      request(3, "consolidation", {
        attempt_failed: { attempt: 1, status: 429 },
      }),
      delta("consolidation_delta", "late"),
    );
    expect(s.streaming.consolidation).toEqual({
      from: 2,
      upto: 8,
      text: "",
      thinking: "",
    });
    s = applyFrame(s, request(4, "consolidation", "first_byte"));
    s = applyFrame(s, delta("consolidation_delta", "retried"));
    expect(s.streaming.consolidation?.text).toBe("retried");
    s = applyFrame(s, checking(5, "checking", 9, 15));
    s = applyFrame(s, request(6, "consolidation", "sent", 2));
    expect(s.streaming.consolidation).toEqual({
      from: 9,
      upto: 15,
      text: "",
      thinking: "",
    });
    s = applyFrame(
      s,
      request(7, "consolidation", { failed: { reason: "stopped" } }, 2),
    );
    expect(s.streaming.consolidation).toBeNull();
    s = applyFrame(s, checking(8));
    s = applyFrame(s, log(9, { Resumed: { gap_ms: 0 } }));
    expect(s.streaming.consolidation).toBeNull();
  });
});
