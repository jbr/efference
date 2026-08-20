import { describe, expect, it } from "vitest";
import type { LogEntry, RawResult } from "../api/types.ts";
import { buildTree, type BatchNode, type CallNode } from "./tree.ts";

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
    Assistant: {
      thinking: opts.thinking === true ? [{ Text: "hm" }] : [],
      text: opts.text === true ? [{ Text: "prose" }] : [],
      calls: (opts.calls ?? []).map((c) => ({ ...c, input: {} })),
    },
  });

const batches = (nodes: ReturnType<typeof buildTree>): BatchNode[] =>
  nodes.filter((n): n is BatchNode => n.kind === "batch");

const onlyCall = (nodes: ReturnType<typeof buildTree>): CallNode =>
  batches(nodes)[0].calls[0];

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
