import { describe, expect, it } from "vitest";
import type { Frame, LogEntry } from "../api/types.ts";
import { messagesAssistant } from "./assistant.ts";
import {
  applyFrame,
  emptySession,
  pushPendingTurn,
  seed,
  seedProcessOutput,
  Store,
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
      Assistant: messagesAssistant([], [{ Text: "hello" }], []),
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

  // Both transport diagnostics are level-triggered: recovery arrives as a `null`
  // frame on each, and the chip surface reads the folded value (docs/transport.md).
  it("folds a retry in flight and honours its all-clear", () => {
    const retry = {
      surface: "turn",
      attempt: 2,
      budget: 12,
      next_attempt_at: 1754300002000,
      message: "attempt 2 of 12 failed upstream, retrying",
      detail: "upstream SSE stream failed mid-stream: connection reset",
    };
    const failing = fold(emptySession, [
      {
        class: "state",
        kind: "upstream_failure",
        data: { status: 529, count: 2 },
      },
      { class: "state", kind: "upstream_retry", data: retry },
    ]);
    expect(failing.topics.upstream_retry).toEqual(retry);
    expect(failing.topics.upstream_failure).toEqual({ status: 529, count: 2 });

    const recovered = fold(failing, [
      { class: "state", kind: "upstream_retry", data: null },
      { class: "state", kind: "upstream_failure", data: null },
    ]);
    expect(recovered.topics.upstream_retry).toBeNull();
    expect(recovered.topics.upstream_failure).toBeNull();
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
          Assistant: messagesAssistant([], [{ Text: "prose" }], []),
        }),
      },
    ]);
    expect(committed.streaming.blocks).toEqual([]);
  });

  // `output` has no replay, so a process already running at connect renders an
  // empty tail until its next chunk — and a backgrounded process is frequently
  // quiet, which is usually why it was backgrounded.
  it("puts a seed read in front of the deltas that arrived after it", () => {
    const s = fold(emptySession, [
      {
        class: "output",
        kind: "process_output",
        data: { id: 3, chunk: "new" },
      },
    ]);
    const seeded = seedProcessOutput(s, 3, "old", 0);
    expect(seeded.streaming.processOutput).toEqual({ 3: "oldnew" });
    expect(seeded.streaming.processLost).toEqual({});
  });

  it("records dropped bytes from the seed read", () => {
    const seeded = seedProcessOutput(emptySession, 3, "tail", 4096);
    expect(seeded.streaming.processLost).toEqual({ 3: 4096 });
  });

  it("leaves the session identical when a seed read is empty", () => {
    expect(seedProcessOutput(emptySession, 3, "", 0)).toBe(emptySession);
  });

  // The orphan: `log` has strict priority over `output` in the daemon's
  // `Tail::next_frame`, so the committing `Assistant` entry can overtake deltas
  // still queued that describe it. Folding those after `blocks` was cleared
  // used to open fresh blocks holding the tail of what just committed — a
  // nameless call block from a trailing `}`, and a duplicate of the message's
  // last sentence. Both survived until the next assistant entry, a whole
  // tool-execution phase later.
  it("drops deltas that arrive after the entry they describe committed", () => {
    const committed = fold(emptySession, [
      {
        class: "output",
        kind: "tool_call_start",
        data: { index: 1, call_id: "toolu_a", name: "bash" },
      },
      {
        class: "log",
        kind: "assistant",
        seq: 1,
        data: entry(1, {
          Assistant: messagesAssistant([], [{ Text: "prose" }], []),
        }),
      },
      // The stragglers, in the order the connection delivers them.
      {
        class: "output",
        kind: "tool_call_input",
        data: { index: 1, fragment: "}" },
      },
      {
        class: "output",
        kind: "assistant_delta",
        data: { index: 0, text: "prose" },
      },
      {
        class: "output",
        kind: "tool_call_start",
        data: { index: 2, call_id: "toolu_b", name: "read" },
      },
    ]);
    expect(committed.streaming.blocks).toEqual([]);
  });

  it("resumes streaming once the next log entry ends the drain", () => {
    const drained = fold(emptySession, [
      {
        class: "log",
        kind: "assistant",
        seq: 1,
        data: entry(1, {
          Assistant: messagesAssistant([], [], []),
        }),
      },
      {
        class: "log",
        kind: "tool_result",
        seq: 2,
        data: entry(2, {
          ToolResult: {
            call_id: "toolu_a",
            raw: { content: "ok", is_error: false },
            wire: "Raw",
          },
        }),
      },
      {
        class: "output",
        kind: "assistant_delta",
        data: { index: 0, text: "next" },
      },
    ]);
    expect(drained.streaming.blocks).toEqual([
      { index: 0, kind: "text", text: "next" },
    ]);
  });

  // Standalone from the drain: only `tool_call_start` carries a call's id and
  // name, so a fragment for an unseen index can only make a nameless call.
  it("never opens a call block from a fragment alone", () => {
    const s = fold(emptySession, [
      {
        class: "output",
        kind: "tool_call_input",
        data: { index: 0, fragment: '{"a"' },
      },
    ]);
    expect(s.streaming.blocks).toEqual([]);
  });

  // Process output is not turn-scoped — a backgrounded process emitting while
  // an assistant entry commits must still reach the tasks pane.
  it("keeps folding process output through the drain", () => {
    const s = fold(emptySession, [
      {
        class: "log",
        kind: "assistant",
        seq: 1,
        data: entry(1, { Assistant: messagesAssistant([], [], []) }),
      },
      {
        class: "output",
        kind: "process_output",
        data: { id: 7, chunk: "tick" },
      },
    ]);
    expect(s.streaming.processOutput).toEqual({ 7: "tick" });
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

/** A `User` entry as the daemon logs a socket submission: the text under the
 * submission id the client (or daemon) minted for the POST. */
const submitted = (seq: number, id: string, text: string): LogEntry => ({
  ...entry(seq, { User: [{ Text: text }] }),
  submission_id: id,
});

describe("pending turns", () => {
  it("settles exactly the turn whose entry arrives, by submission id", () => {
    const queued = pushPendingTurn(
      pushPendingTurn(emptySession, "sub-a", "first"),
      "sub-b",
      "second",
    );
    const s = fold(queued, [logFrame(submitted(1, "sub-a", "first"))]);
    expect(s.pendingTurns).toEqual([
      { id: "sub-b", text: "second", attachments: [] },
    ]);
  });

  it("leaves the queue alone for an entry with someone else's id", () => {
    const queued = pushPendingTurn(emptySession, "sub-mine", "mine");
    const s = fold(queued, [
      logFrame(submitted(1, "sub-other", "someone else's")),
    ]);
    expect(s.pendingTurns).toEqual([
      { id: "sub-mine", text: "mine", attachments: [] },
    ]);
  });

  it("settles identical texts independently — ids, not content, key them", () => {
    const queued = pushPendingTurn(
      pushPendingTurn(emptySession, "sub-1", "go"),
      "sub-2",
      "go",
    );
    const once = fold(queued, [logFrame(submitted(1, "sub-2", "go"))]);
    expect(once.pendingTurns).toEqual([
      { id: "sub-1", text: "go", attachments: [] },
    ]);
    const twice = fold(once, [logFrame(submitted(2, "sub-1", "go"))]);
    expect(twice.pendingTurns).toEqual([]);
  });

  it("ignores entries with no submission id", () => {
    const queued = pushPendingTurn(emptySession, "sub-x", "pending");
    const s = fold(queued, [
      logFrame(entry(1, { User: [{ Text: "pending" }] })),
    ]);
    expect(s.pendingTurns).toEqual([
      { id: "sub-x", text: "pending", attachments: [] },
    ]);
  });

  it("skips the push when the entry already folded ahead of the 202", () => {
    // The submit response can lose the race to the entry's own log frame;
    // the id makes "already delivered" an exact check.
    const folded = fold(emptySession, [
      logFrame(submitted(1, "sub-r", "raced")),
    ]);
    const s = pushPendingTurn(folded, "sub-r", "raced");
    expect(s.pendingTurns).toEqual([]);
  });

  it("settles from a re-seed that absorbed the User entry", () => {
    const queued = pushPendingTurn(emptySession, "sub-s", "reseeded");
    const s = seed(queued, [submitted(1, "sub-s", "reseeded")]);
    expect(s.pendingTurns).toEqual([]);
  });
});

// The defect these pin: a long block arrives as thousands of deltas, and a
// notification per delta is a render per delta. The render re-parses the whole
// growing block as markdown — linear in block length, so quadratic over the
// block — and once a delta costs more than the interval between deltas the
// backlog outlives the stream: the frames already buffered in the fetch body
// keep folding at full length after the producer has gone quiet, which is why
// the page never recovered on its own (backlog/web/streaming-hot-loop.md).
describe("coalescing the fold", () => {
  /** A scheduler under the test's control: nothing flushes until it says so. */
  function manual() {
    const queued: (() => void)[] = [];
    return {
      schedule: (flush: () => void) => queued.push(flush),
      /** How many flushes were asked for — one per batch, not one per fold. */
      get scheduled() {
        return queued.length;
      },
      run() {
        for (const flush of queued.splice(0)) flush();
      },
    };
  }

  const delta = (index: number, text: string): Frame => ({
    class: "output",
    kind: "assistant_delta",
    data: { index, text },
  });

  it("turns a run of deltas into one notification", () => {
    const clock = manual();
    const store = new Store(clock.schedule);
    let notified = 0;
    store.subscribe(() => notified++);
    for (let i = 0; i < 2000; i++)
      store.update((s) => applyFrame(s, delta(0, "x ")));
    // The fold is synchronous — everything is already in the snapshot — but
    // nothing has re-rendered yet.
    expect(store.snapshot().streaming.blocks[0].text).toHaveLength(4000);
    expect(notified).toBe(0);
    expect(clock.scheduled).toBe(1);
    clock.run();
    expect(notified).toBe(1);
  });

  it("schedules again for the next batch, and only for a fold that changed", () => {
    const clock = manual();
    const store = new Store(clock.schedule);
    let notified = 0;
    store.subscribe(() => notified++);
    store.update((s) => applyFrame(s, delta(0, "a")));
    clock.run();
    store.update((s) => applyFrame(s, delta(0, "b")));
    clock.run();
    expect(notified).toBe(2);
    // A fold that returns the same session is not a change and owes nothing.
    store.update((s) => s);
    expect(clock.scheduled).toBe(0);
    clock.run();
    expect(notified).toBe(2);
  });

  it("flushes on demand for a caller that needs the render to have happened", () => {
    // The dev scrubber steps to one position and looks at it; waiting a frame
    // would make `step()` from the console land after the command returned.
    const clock = manual();
    const store = new Store(clock.schedule);
    let notified = 0;
    store.subscribe(() => notified++);
    store.update((s) => applyFrame(s, delta(0, "a")));
    store.flush();
    expect(notified).toBe(1);
    // The scheduled flush finds nothing owed rather than notifying twice.
    clock.run();
    expect(notified).toBe(1);
  });

  it("folds a long block's deltas well inside one frame's budget", () => {
    // The fold itself is string append and a shallow copy; the cost the defect
    // was about lives in the render the notification triggers. If this bound
    // ever fails, the fold grew a per-delta pass over the block.
    const store = new Store(() => {});
    const started = performance.now();
    for (let i = 0; i < 4000; i++) {
      store.update((s) => applyFrame(s, delta(0, "some prose ")));
    }
    const elapsed = performance.now() - started;
    expect(store.snapshot().streaming.blocks[0].text).toHaveLength(44000);
    expect(elapsed).toBeLessThan(150);
  });
});

describe("the call↔process mapping", () => {
  const processesFrame = (procs: unknown[]): Frame =>
    ({
      class: "state",
      kind: "processes",
      data: procs,
    }) as unknown as Frame;

  it("learns which process answers a call from the processes topic", () => {
    const s = applyFrame(
      emptySession,
      processesFrame([
        { id: 3, command: "npm run dev", call_id: "toolu_a", background: true },
        { id: 4, command: "cargo build", call_id: null, background: false },
      ]),
    );
    expect(s.processCalls).toEqual({ toolu_a: 3 });
  });

  // The topic is level-triggered and drops a process the moment it exits, but the
  // retained output stays readable at `GET /processes/:id/output` — so the mapping
  // has to outlive the row that taught it, or a finished backgrounded command's
  // output becomes unreachable from its transcript entry.
  it("keeps a pair after the process leaves the topic", () => {
    const s = fold(emptySession, [
      processesFrame([{ id: 3, command: "npm run dev", call_id: "toolu_a" }]),
      processesFrame([]),
    ]);
    expect(s.processCalls).toEqual({ toolu_a: 3 });
  });

  it("returns the same object when a topic frame teaches it nothing", () => {
    const first = applyFrame(
      emptySession,
      processesFrame([{ id: 3, command: "x", call_id: "toolu_a" }]),
    );
    const second = applyFrame(
      first,
      processesFrame([{ id: 3, command: "x", call_id: "toolu_a" }]),
    );
    expect(second.processCalls).toBe(first.processCalls);
  });
});
