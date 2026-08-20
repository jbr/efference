import { describe, expect, it } from "vitest";
import type { Frame, LogEntry } from "../api/types.ts";
import {
  applyFrame,
  emptySession,
  pushPendingTurn,
  seed,
  type Session,
} from "./store.ts";

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
  it("accumulates deltas into the block each one names", () => {
    const s = fold(emptySession, [
      {
        class: "output",
        kind: "thinking_delta",
        data: { index: 0, text: "hm" },
      },
      {
        class: "output",
        kind: "assistant_delta",
        data: { index: 1, text: "I'll " },
      },
      {
        class: "output",
        kind: "assistant_delta",
        data: { index: 1, text: "look" },
      },
      {
        class: "output",
        kind: "tool_call_start",
        data: { index: 2, call_id: "toolu_a", name: "bash" },
      },
      {
        class: "output",
        kind: "tool_call_input",
        data: { index: 2, fragment: '{"cmd' },
      },
    ]);
    expect(s.streaming.blocks).toEqual([
      { index: 0, kind: "thinking", text: "hm" },
      { index: 1, kind: "text", text: "I'll look" },
      {
        index: 2,
        kind: "call",
        text: '{"cmd',
        callId: "toolu_a",
        name: "bash",
      },
    ]);
  });

  // The defect the block scoping exists for: with one buffer per kind, the
  // second call's start reset the shared input buffer and the first call's
  // arriving arguments vanished from the display (docs/control-api.md).
  it("keeps two calls in one response distinct", () => {
    const s = fold(emptySession, [
      {
        class: "output",
        kind: "tool_call_start",
        data: { index: 0, call_id: "toolu_a", name: "read" },
      },
      {
        class: "output",
        kind: "tool_call_input",
        data: { index: 0, fragment: '{"paths"' },
      },
      {
        class: "output",
        kind: "tool_call_start",
        data: { index: 1, call_id: "toolu_b", name: "bash" },
      },
      {
        class: "output",
        kind: "tool_call_input",
        data: { index: 1, fragment: '{"command"' },
      },
    ]);
    expect(s.streaming.blocks).toEqual([
      {
        index: 0,
        kind: "call",
        text: '{"paths"',
        callId: "toolu_a",
        name: "read",
      },
      {
        index: 1,
        kind: "call",
        text: '{"command"',
        callId: "toolu_b",
        name: "bash",
      },
    ]);
  });

  it("clears the assistant buffers when the assistant entry commits", () => {
    const streamed = fold(emptySession, [
      {
        class: "output",
        kind: "assistant_delta",
        data: { index: 0, text: "prose" },
      },
      {
        class: "output",
        kind: "tool_call_start",
        data: { index: 1, call_id: "toolu_a", name: "bash" },
      },
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
    expect(committed.streaming.blocks).toEqual([]);
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

describe("pending turns", () => {
  it("settles the first content match when the User entry arrives", () => {
    const queued = pushPendingTurn(
      pushPendingTurn(emptySession, "first", null),
      "second",
      null,
    );
    const s = fold(queued, [logFrame(entry(1, { User: [{ Text: "first" }] }))]);
    expect(s.pendingTurns).toEqual(["second"]);
  });

  it("leaves the queue alone for a User entry that matches nothing", () => {
    const queued = pushPendingTurn(emptySession, "mine", null);
    const s = fold(queued, [
      logFrame(entry(1, { User: [{ Text: "someone else's" }] })),
    ]);
    expect(s.pendingTurns).toEqual(["mine"]);
  });

  it("settles identical turns one at a time, in order", () => {
    const queued = pushPendingTurn(
      pushPendingTurn(emptySession, "go", null),
      "go",
      null,
    );
    const once = fold(queued, [logFrame(entry(1, { User: [{ Text: "go" }] }))]);
    expect(once.pendingTurns).toEqual(["go"]);
    const twice = fold(once, [logFrame(entry(2, { User: [{ Text: "go" }] }))]);
    expect(twice.pendingTurns).toEqual([]);
  });

  it("ignores non-User entries", () => {
    const queued = pushPendingTurn(emptySession, "pending", null);
    const s = fold(queued, [
      logFrame(entry(1, { Assistant: { thinking: [], text: [], calls: [] } })),
    ]);
    expect(s.pendingTurns).toEqual(["pending"]);
  });

  it("skips the push when the entry already folded ahead of the 202", () => {
    // The submit response can lose the race to the entry's own log frame; the
    // pre-submit cursor (seq 4 here) marks the entry as this turn's delivery.
    const folded = fold(seed(emptySession, [entry(4, { Assistant: { thinking: [], text: [], calls: [] } })]), [
      logFrame(entry(5, { User: [{ Text: "raced" }] })),
    ]);
    const s = pushPendingTurn(folded, "raced", 4);
    expect(s.pendingTurns).toEqual([]);
  });

  it("still queues when the only content match predates the submit", () => {
    // An identical turn sent earlier in the session is not this submission's
    // delivery: entries at or below the pre-submit cursor don't count.
    const folded = fold(emptySession, [
      logFrame(entry(1, { User: [{ Text: "again" }] })),
    ]);
    const s = pushPendingTurn(folded, "again", 1);
    expect(s.pendingTurns).toEqual(["again"]);
  });

  it("settles from a re-seed that absorbed the User entry", () => {
    const queued = pushPendingTurn(
      fold(emptySession, [logFrame(entry(1, { Assistant: { thinking: [], text: [], calls: [] } }))]),
      "reseeded",
      1,
    );
    const s = seed(queued, [
      entry(1, { Assistant: { thinking: [], text: [], calls: [] } }),
      entry(2, { User: [{ Text: "reseeded" }] }),
    ]);
    expect(s.pendingTurns).toEqual([]);
  });

  it("does not settle from seeded entries already behind the cursor", () => {
    // A tail window mostly re-reads entries the fold has seen; an old identical
    // User entry among them is not this pending turn's delivery.
    const seen = fold(emptySession, [
      logFrame(entry(1, { User: [{ Text: "echo" }] })),
    ]);
    const queued = pushPendingTurn(seen, "echo", 1);
    const s = seed(queued, [entry(1, { User: [{ Text: "echo" }] })]);
    expect(s.pendingTurns).toEqual(["echo"]);
  });
});
