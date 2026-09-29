import { describe, expect, it } from "vitest";
import { KICKOFF_SOURCE, type LogEntry, type RawResult } from "../api/types.ts";
import {
  buildTree,
  upstreamFailureWhy,
  type BatchNode,
  type CallNode,
} from "./tree.ts";
import { messagesAssistant } from "./assistant.ts";

const entry = (seq: number, payload: LogEntry["payload"]): LogEntry => ({
  seq,
  turn: 1,
  at: 1754300000000,
  payload,
});

const raw: RawResult = { content: "ok", is_error: false };

const assistant = (
  seq: number,
  opts: {
    thinking?: boolean;
    text?: boolean;
    calls?: { id: string; name: string }[];
  },
) =>
  entry(seq, {
    Assistant: messagesAssistant(
      opts.thinking === true ? [{ Text: "hm" }] : [],
      opts.text === true ? [{ Text: "prose" }] : [],
      (opts.calls ?? []).map((c) => ({ ...c, input: {} })),
    ),
  });

const batches = (nodes: ReturnType<typeof buildTree>): BatchNode[] =>
  nodes.filter((n): n is BatchNode => n.kind === "batch");

const onlyCall = (nodes: ReturnType<typeof buildTree>): CallNode =>
  batches(nodes)[0].calls[0];

describe("record-only entries", () => {
  // An entry the stream never stood behind is not in the model's context, and
  // must not read like a turn that is (docs/transport.md).
  it("leads with a marker node, above content that still renders", () => {
    const nodes = buildTree([
      entry(1, {
        Assistant: {
          ...messagesAssistant([], [{ Text: "half a sentence" }], []),
          upstream_failure: "stream aborted after 2 blocks",
        },
      }),
    ]);
    expect(nodes.map((n) => n.key)).toEqual(["1:aborted", "1:text"]);
  });

  it("is visible even when the entry is empty — the empty-stream case", () => {
    const nodes = buildTree([
      entry(1, {
        Assistant: {
          ...messagesAssistant([], [], []),
          upstream_failure: { why: "empty finalized stream", stream: {} },
        },
      }),
    ]);
    expect(nodes.map((n) => n.key)).toEqual(["1:aborted"]);
  });

  it("reads the why out of either serialization", () => {
    expect(upstreamFailureWhy("aborted")).toBe("aborted");
    expect(upstreamFailureWhy({ why: "aborted", stream: {} })).toBe("aborted");
  });

  it("leaves a finalized turn unmarked", () => {
    expect(buildTree([assistant(1, { text: true })]).map((n) => n.key)).toEqual(
      ["1:text"],
    );
  });
});

describe("the transcript tree", () => {
  it("keeps prose and calls as siblings, not nested", () => {
    // A step's thinking, prose and calls are separate things to read. `step`
    // is deliberately not a container: nesting on it would put the prose a
    // level deeper than the calls issued in the same breath.
    const nodes = buildTree([
      assistant(1, {
        thinking: true,
        text: true,
        calls: [{ id: "a", name: "read" }],
      }),
    ]);
    expect(nodes.map((n) => n.kind)).toEqual(["leaf", "leaf", "batch"]);
  });

  it("attaches a result and a verdict to the call they name", () => {
    const nodes = buildTree([
      assistant(1, { calls: [{ id: "toolu_a", name: "read" }] }),
      entry(2, {
        Spot: { call_id: "toolu_a", stance: "clear", reason: "in scope" },
      } as LogEntry["payload"]),
      entry(3, { ToolResult: { call_id: "toolu_a", raw, wire: "Raw" } }),
    ]);
    // One batch and nothing else: the result and the verdict are not siblings
    // of the call, they are part of it.
    expect(nodes).toHaveLength(1);
    const call = onlyCall(nodes);
    expect(call.callId).toBe("toolu_a");
    expect(call.spot?.stance).toBe("clear");
    expect(call.result?.seq).toBe(3);
  });

  it("attaches tool telemetry to the call it names, never as a leaf", () => {
    // Regression: the `ToolTelemetry` variant shipped after this client's
    // `Payload` union was written, so it fell through to the unknown-entry
    // renderer and every call grew a stray `{"ToolTelemetry":{…}}` block in the
    // transcript. It is a fact about a call, like `Spot`, not an entry to read.
    const nodes = buildTree([
      assistant(1, { calls: [{ id: "toolu_a", name: "list" }] }),
      entry(2, {
        ToolTelemetry: {
          call_id: "toolu_a",
          tool: "list",
          duration_ms: 4,
          outcome: "applied",
          result_bytes: 279,
        },
      } as LogEntry["payload"]),
    ]);
    expect(nodes).toHaveLength(1);
    const call = onlyCall(nodes);
    expect(call.telemetry?.duration_ms).toBe(4);
    expect(call.telemetry?.result_bytes).toBe(279);
  });

  it("keeps a process lifecycle record out of the transcript", () => {
    // The mint and the exit are already in the transcript as prose — the
    // backgrounding result and the exit notification. The record behind them
    // (docs/supervisor.md) is what `GET /processes/:id` reads; rendering it
    // would put a raw-JSON block beside every backgrounded command.
    const nodes = buildTree([
      assistant(1, { calls: [{ id: "toolu_a", name: "bash" }] }),
      entry(2, {
        ProcessEvent: {
          id: 1,
          kind: {
            Spawned: {
              command: "cargo watch -x test",
              call_id: "toolu_a",
              pid: 4242,
              at: 1754300000000,
            },
          },
        },
      }),
      entry(3, {
        ProcessEvent: {
          id: 1,
          kind: {
            Exited: {
              outcome: { code: 0 },
              duration_ms: 12000,
              at: 1754300012000,
            },
          },
        },
      }),
    ]);
    expect(nodes).toHaveLength(1);
    expect(nodes.filter((n) => n.kind === "leaf")).toHaveLength(0);
  });

  it("groups the calls of one step into one batch", () => {
    const nodes = buildTree([
      assistant(1, {
        calls: [
          { id: "toolu_a", name: "read" },
          { id: "toolu_b", name: "bash" },
        ],
      }),
      entry(2, { ToolResult: { call_id: "toolu_b", raw, wire: "Raw" } }),
      entry(3, { ToolResult: { call_id: "toolu_a", raw, wire: "Raw" } }),
    ]);
    const [batch] = batches(nodes);
    expect(batch.calls.map((c) => c.callId)).toEqual(["toolu_a", "toolu_b"]);
    // Results attach to their own call regardless of the order they land in.
    expect(batch.calls[0].result?.seq).toBe(3);
    expect(batch.calls[1].result?.seq).toBe(2);
  });

  it("gives each step its own batch", () => {
    const nodes = buildTree([
      assistant(1, { calls: [{ id: "a", name: "read" }] }),
      entry(2, { ToolResult: { call_id: "a", raw, wire: "Raw" } }),
      assistant(3, { calls: [{ id: "b", name: "bash" }] }),
    ]);
    expect(batches(nodes)).toHaveLength(2);
  });

  it("does not render triage: the result header already states the count", () => {
    const nodes = buildTree([
      assistant(1, { calls: [{ id: "a", name: "read" }] }),
      entry(2, { ToolResult: { call_id: "a", raw, wire: "Raw" } }),
      entry(3, {
        Triage: {
          for_result: 2,
          origin: "Model",
          raw_keep: "all",
          inbox: [],
          surfaced: [],
          retries: [],
        },
      } as LogEntry["payload"]),
    ]);
    expect(nodes).toHaveLength(1);
    expect(nodes[0].kind).toBe("batch");
  });

  it("keeps user turns and other entries as session-level leaves", () => {
    const nodes = buildTree([
      entry(1, { User: [{ Text: "hi" }] }),
      entry(2, {
        Mark: { kind: { Boundary: { upto: 1 } }, note: "a breadcrumb" },
      }),
    ]);
    expect(nodes.map((n) => n.kind)).toEqual(["leaf", "leaf"]);
  });

  it("drops a result whose call is not in the window", () => {
    // A tail window can begin after the call that a result names. Rendering it
    // loose would show output with nothing to attribute it to.
    const nodes = buildTree([
      entry(9, { ToolResult: { call_id: "gone", raw, wire: "Raw" } }),
      entry(10, { User: [{ Text: "hi" }] }),
    ]);
    expect(nodes).toHaveLength(1);
    expect(nodes[0].kind).toBe("leaf");
  });

  it("is a pure function of the entries", () => {
    const entries = [
      assistant(1, { calls: [{ id: "a", name: "read" }] }),
      entry(2, { ToolResult: { call_id: "a", raw, wire: "Raw" } }),
    ];
    expect(buildTree(entries)).toEqual(buildTree(entries));
  });
});

describe("the consolidation mark", () => {
  it("counts the committed entries at or below its boundary", () => {
    const nodes = buildTree([
      entry(1, { User: [{ Text: "hi" }] }),
      assistant(2, { text: true }),
      entry(3, { User: [{ Text: "more" }] }),
      entry(4, {
        Mark: { kind: { Boundary: { upto: 3 } }, note: "summary" },
      }),
    ]);
    const mark = nodes.at(-1);
    expect(mark?.kind).toBe("leaf");
    expect(mark?.kind === "leaf" && mark.replaces).toBe(3);
  });

  it("excludes entries past the boundary", () => {
    const nodes = buildTree([
      entry(1, { User: [{ Text: "hi" }] }),
      entry(2, { User: [{ Text: "later" }] }),
      entry(3, {
        Mark: { kind: { Boundary: { upto: 1 } }, note: "summary" },
      }),
    ]);
    const mark = nodes.at(-1);
    expect(mark?.kind === "leaf" && mark.replaces).toBe(1);
  });
});

describe("title and preamble entries", () => {
  it("renders a title only when its text changes, and never the preamble", () => {
    const title = (seq: number, text: string) =>
      entry(seq, { Title: { text, author: "triage" } });
    const nodes = buildTree([
      entry(0, { Preamble: { system: "you are…", context: null } }),
      title(1, "Probe the models endpoint"),
      title(2, "Probe the models endpoint"),
      title(3, "Build efference dev models"),
    ]);
    expect(nodes.map((n) => n.key)).toEqual(["1", "3"]);
  });
});

describe("the kickoff", () => {
  // The harness's first-turn belongs to the session's opening, like the
  // preamble, so the agent's greeting leads the transcript (docs/context.md).
  it("is not drawn, while other sourced turns are", () => {
    const nodes = buildTree([
      {
        ...entry(1, { User: [{ Text: "greet them" }] }),
        source: KICKOFF_SOURCE,
      },
      assistant(2, { text: true }),
      { ...entry(3, { User: [{ Text: "from mcp" }] }), source: "mcp[cc]" },
    ]);
    expect(nodes.map((n) => n.key)).toEqual(["2:text", "3"]);
  });
});

describe("ask", () => {
  const ask = (id: string, question: string) => ({
    id,
    name: "ask",
    input: { question },
  });
  const asking = (
    seq: number,
    calls: { id: string; name: string; input: unknown }[],
  ) => entry(seq, { Assistant: messagesAssistant([], [], calls) });
  const reply = (
    seq: number,
    callId: string,
    submission?: string,
  ): LogEntry => ({
    ...entry(seq, {
      ToolResult: {
        call_id: callId,
        raw: { content: "yes, ship", is_error: submission === undefined },
        wire: "Raw",
      },
    }),
    ...(submission === undefined ? {} : { submission_id: submission }),
  });

  it("is drawn as the question, with no call and no batch", () => {
    const nodes = buildTree([asking(1, [ask("a", "Ship it?")])]);
    expect(nodes.map((n) => n.key)).toEqual(["1:ask:a"]);
    const leaf = nodes[0];
    expect(leaf.kind === "leaf" && leaf.ask).toEqual({
      callId: "a",
      question: "Ship it?",
      n: 1,
      of: 1,
    });
  });

  it("numbers several asks and puts them after the step's other calls", () => {
    const nodes = buildTree([
      asking(1, [
        ask("a", "First?"),
        { id: "b", name: "bash", input: { command: "ls" } },
        ask("c", "Second?"),
      ]),
    ]);
    expect(nodes.map((n) => n.key)).toEqual(["1:batch", "1:ask:a", "1:ask:c"]);
    const asks = nodes.flatMap((n) =>
      n.kind === "leaf" && n.ask ? [n.ask] : [],
    );
    expect(asks.map((a) => [a.n, a.of])).toEqual([
      [1, 2],
      [2, 2],
    ]);
  });

  it("reads a result carrying a submission id as the operator's reply", () => {
    const nodes = buildTree([
      asking(1, [ask("a", "Ship it?")]),
      reply(2, "a", "sub-1"),
    ]);
    expect(nodes.map((n) => n.key)).toEqual(["1:ask:a", "2:reply"]);
    const leaf = nodes[1];
    expect(leaf.kind === "leaf" && leaf.reply).toBe("answered");
    const question = nodes[0];
    expect(question.kind === "leaf" && question.ask?.resolved).toBe(true);
  });

  it("reads a result with no submission as not answered", () => {
    const nodes = buildTree([asking(1, [ask("a", "Ship it?")]), reply(2, "a")]);
    const leaf = nodes[1];
    expect(leaf.kind === "leaf" && leaf.reply).toBe("unanswered");
  });

  it("leaves an ask with no usable question an ordinary call", () => {
    const nodes = buildTree([
      asking(1, [{ id: "a", name: "ask", input: {} }]),
      reply(2, "a"),
    ]);
    expect(nodes.map((n) => n.key)).toEqual(["1:batch"]);
    expect(onlyCall(nodes).result?.seq).toBe(2);
  });
});
