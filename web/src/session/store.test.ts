import { describe, expect, it } from "vitest";
import type { Frame, LogEntry } from "../api/types.ts";
import { applyFrame, emptySession, seed, type Session } from "./store.ts";

const entry = (seq: number, payload: LogEntry["payload"]): LogEntry => ({
  seq,
  turn: 1,
  at: 1754300000000,
  payload,
});

const logFrame = (e: LogEntry): Frame => ({
  class: "log",
  kind: "user",
  seq: e.seq,
  data: e,
});

const fold = (session: Session, frames: Frame[]) =>
  frames.reduce(applyFrame, session);

describe("the log fold", () => {
  it("appends entries in order and advances the cursor", () => {
    const a = entry(1, { User: [{ Text: "hi" }] });
    const b = entry(2, {
      Assistant: { thinking: [], text: [{ Text: "hello" }], calls: [] },
    });
    const s = fold(emptySession, [logFrame(a), logFrame(b)]);
    expect(s.entries).toEqual([a, b]);
    expect(s.lastSeq).toBe(2);
  });

  it("is idempotent under replay: a seq at or below the cursor is a no-op", () => {
    const a = entry(1, { User: [{ Text: "hi" }] });
    const once = fold(emptySession, [logFrame(a)]);
    expect(fold(once, [logFrame(a)])).toBe(once);
  });

  it("seeds from a tail window", () => {
    const s = seed(emptySession, [
      entry(41, { User: [{ Text: "a" }] }),
      entry(42, { User: [{ Text: "b" }] }),
    ]);
    expect(s.lastSeq).toBe(42);
  });
});

describe("the state fold", () => {
  it("replaces a topic wholesale and clears on null", () => {
    const gate = { call_id: "toolu_1", name: "bash", input: {} };
    const set: Frame = { class: "state", kind: "gate", data: gate };
    const clear: Frame = { class: "state", kind: "gate", data: null };
    const s = fold(emptySession, [set]);
    expect(s.topics.gate).toEqual(gate);
    expect(fold(s, [clear]).topics.gate).toBeNull();
  });

  it("is order-insensitive across distinct topics", () => {
    const running: Frame = { class: "state", kind: "running", data: true };
    const pace: Frame = {
      class: "state",
      kind: "pace",
      data: { speed_ms: 15000, held: false },
    };
    expect(fold(emptySession, [running, pace]).topics).toEqual(
      fold(emptySession, [pace, running]).topics,
    );
  });
});

describe("the output tail", () => {
  it("accumulates deltas per kind", () => {
    const s = fold(emptySession, [
      { class: "output", kind: "thinking_delta", data: { text: "hm" } },
      { class: "output", kind: "assistant_delta", data: { text: "I'll " } },
      { class: "output", kind: "assistant_delta", data: { text: "look" } },
      { class: "output", kind: "tool_call_start", data: { name: "bash" } },
      { class: "output", kind: "tool_call_input", data: { fragment: '{"cmd' } },
    ]);
    expect(s.streaming.thinking).toBe("hm");
    expect(s.streaming.assistant).toBe("I'll look");
    expect(s.streaming.toolName).toBe("bash");
    expect(s.streaming.toolInput).toBe('{"cmd');
  });

  it("clears the assistant buffers when the assistant entry commits", () => {
    const streamed = fold(emptySession, [
      { class: "output", kind: "assistant_delta", data: { text: "prose" } },
      { class: "output", kind: "tool_call_start", data: { name: "bash" } },
    ]);
    const committed = fold(streamed, [
      {
        class: "log",
        kind: "assistant",
        seq: 1,
        data: entry(1, {
          Assistant: { thinking: [], text: [{ Text: "prose" }], calls: [] },
        }),
      },
    ]);
    expect(committed.streaming.assistant).toBe("");
    expect(committed.streaming.toolName).toBeNull();
  });

  it("clears a call's live output when its result commits, keeping others", () => {
    const streamed = fold(emptySession, [
      {
        class: "output",
        kind: "tool_output",
        data: { call_id: "toolu_a", chunk: "aaa" },
      },
      {
        class: "output",
        kind: "tool_output",
        data: { call_id: "toolu_b", chunk: "bbb" },
      },
    ]);
    const committed = fold(streamed, [
      {
        class: "log",
        kind: "tool_result",
        seq: 1,
        data: entry(1, {
          ToolResult: {
            call_id: "toolu_a",
            raw: { content: "aaa", is_error: false },
            wire: "Raw",
          },
        }),
      },
    ]);
    expect(committed.streaming.toolOutput).toEqual({ toolu_b: "bbb" });
  });
});
