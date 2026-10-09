// The transcript hierarchy (docs/web-client.md): a pure projection of the flat
// log into the containment the display renders. This is to the web client what
// `tui/projection.rs` is to the TUI — a second projection of the same log to a
// different target — and it lives beside the fold rather than inside components
// so it can be tested as data.
//
//   session
//   ├─ user text / thinking / agent text / mark / ambient / config-edit / preamble
//   └─ batch                    the calls issued in one step
//      └─ call                  one call, whole
//         ├─ args, intent       (rendered from the call itself)
//         ├─ spot               the spotter's stance + reason
//         └─ result             carries its own keep + note
//
// Two containers, `batch` and `call`; everything else is a leaf sibling at
// session level. What is deliberately *not* here: any per-user-message
// grouping. An operator interjection is often incidental ("check this path: …"),
// and mid-trajectory injection (docs/control-channel.md) makes those normal
// rather than rare, so hanging subsequent calls off the message that preceded
// them would encode a causality that is usually absent. `step` is likewise a
// real boundary that is not nested on: it would put agent prose a level deeper
// than the calls issued in the same breath, and the prose is often the answer.

import {
  KICKOFF_SOURCE,
  type CallId,
  type LogEntry,
  type Seq,
  type Spot,
  type ToolCall,
  type ToolTelemetry,
  type UpstreamFailureRecord,
} from "../api/types.ts";
import { assistantView } from "./assistant.ts";

/** The prose half of a record-only mark, whichever shape the log wrote
 * (src/log.rs): the bare string older logs carry, or the object's `why`. */
export const upstreamFailureWhy = (record: UpstreamFailureRecord): string =>
  typeof record === "string" ? record : record.why;

/** A leaf: one log entry rendered on its own, at session level. */
export interface LeafNode {
  kind: "leaf";
  key: string;
  entry: LogEntry;
  /** Marks only: how many committed entries sit at or below the boundary's
   * `upto` — the span the consolidation replaced on the wire. Client-computed
   * from the window; the wire's token delta is provider accounting the client
   * does not hold, so the count is entries, not tokens. */
  replaces?: number;
  /** An `ask` call drawn as the assistant's question (docs/web-client.md): the
   * entry is the `Assistant` entry that issued it. */
  ask?: AskQuestion;
  /** The result of an `ask`: the operator's reply when a submission produced
   * it, or a note that nothing did. The entry is the `ToolResult`. */
  reply?: "answered" | "unanswered";
}

/** A question put to the operator, and its place among the asks of one
 * completion. */
export interface AskQuestion {
  /** The `ask` call's id — what the live `ask` topic's `call_id` names. */
  callId: CallId;
  question: string;
  n: number;
  of: number;
  /** Its result has committed, so the topic no longer says anything about it. */
  resolved?: boolean;
}

/** The question an `ask` call puts, when `calls[index]` is one with a usable
 * `question` — `supervisor::ask_of_call` in src/supervisor.rs. Anything else,
 * including an ask the loop answers with an error, stays an ordinary call. */
export function askOfCall(
  calls: ToolCall[],
  index: number,
): AskQuestion | null {
  const call = calls[index];
  if (call?.name !== "ask") return null;
  const input = call.input as { question?: unknown } | null;
  const question =
    typeof input?.question === "string" ? input.question.trim() : "";
  if (question === "") return null;
  const asks = calls.filter((c) => c.name === "ask");
  return {
    callId: call.id,
    question,
    n: calls.slice(0, index).filter((c) => c.name === "ask").length + 1,
    of: asks.length,
  };
}

/** One call, with everything the harness said about it. */
export interface CallNode {
  kind: "call";
  key: string;
  callId: CallId;
  name: string;
  /** The `Assistant` entry that issued this call — the source of its input. */
  issued: LogEntry;
  /** The spotter's verdict on this call, once one is logged. */
  spot: Spot | null;
  /** The `ToolResult` entry, once the call has returned. A result captured
   * `Pending` is replaced here by a copy carrying its `ResultVerdict`'s wire
   * when that lands. */
  result: LogEntry | null;
  /** What the dispatch cost and how it came out, once logged. Folded onto the
   * call rather than rendered as a leaf for the same reason `Spot` is: it is a
   * fact *about* this call, and the log's ordering already ties it to one. */
  telemetry: ToolTelemetry | null;
}

/** The calls issued in one step, grouped. Present even for a single call: the
 * node is what gives depth-based styling and per-batch state somewhere to live,
 * and a batch of one renders as one call. */
export interface BatchNode {
  kind: "batch";
  key: string;
  calls: CallNode[];
}

export type Node = LeafNode | BatchNode | CallNode;

/** An `Assistant` entry contributes up to three things in wire order: its
 * thinking, its prose, and the batch of calls it issued. They are separate
 * nodes because they are separate things to read, not because the wire splits
 * them. */
export type TranscriptNode = LeafNode | BatchNode;

/**
 * Project the committed log into the tree. Pure, and a function of the entries
 * alone — the pre-commit tail is layered on by the renderer, not folded in
 * here, so this stays testable as data.
 *
 * Results and verdicts are attached to the call they name rather than emitted
 * where they appear in the log: that attachment is the whole point of the
 * hierarchy. Entries naming a call that isn't in the log are dropped rather
 * than rendered loose — a result with no call is not a thing a reader can act
 * on, and the stream's coherence rule (docs/control-api.md) means it does not
 * normally happen.
 */
export function buildTree(entries: LogEntry[]): TranscriptNode[] {
  const nodes: TranscriptNode[] = [];
  const byCall = new Map<CallId, CallNode>();
  // Calls drawn as questions: their results are replies, not tool results.
  const askCalls = new Map<CallId, AskQuestion>();
  // Results captured `Pending`, by their seq: the slots a `ResultVerdict` may
  // still decide. The first non-Pending verdict removes its entry, so a later
  // one is ignored (mirrors `result_verdicts`, src/log/curation.rs).
  const pending = new Map<Seq, CallNode>();
  const seqs: Seq[] = [];
  let title: string | null = null;

  for (const entry of entries) {
    const p = entry.payload;
    seqs.push(entry.seq);

    // The kickoff is part of the session's opening, like the preamble: not drawn
    // as a turn, so the agent's greeting leads the transcript (docs/context.md).
    if ("User" in p && entry.source === KICKOFF_SOURCE) continue;

    if ("Mark" in p) {
      // Tolerant of a mark kind this client does not know: it renders as a
      // plain note rather than a boundary with an extent.
      const upto =
        typeof p.Mark.kind === "object" && "Boundary" in p.Mark.kind
          ? p.Mark.kind.Boundary.upto
          : null;
      nodes.push({
        kind: "leaf",
        key: `${entry.seq}`,
        entry,
        replaces:
          upto === null ? undefined : seqs.filter((s) => s <= upto).length,
      });
      continue;
    }

    if ("Assistant" in p) {
      const { upstream_failure } = p.Assistant;
      const { thinking, text, calls } = assistantView(p.Assistant);
      // A record-only entry leads with the marker, above content that still
      // renders — it is often the only copy of what the model produced. The
      // node exists even when the entry is empty, which is exactly the
      // empty-finalized-stream case: otherwise it renders as nothing at all.
      if (upstream_failure !== undefined) {
        nodes.push({ kind: "leaf", key: `${entry.seq}:aborted`, entry });
      }
      if (thinking.length > 0) {
        nodes.push({ kind: "leaf", key: `${entry.seq}:thinking`, entry });
      }
      if (text.length > 0) {
        nodes.push({ kind: "leaf", key: `${entry.seq}:text`, entry });
      }
      const asks = calls.flatMap((call, i) => {
        const ask = askOfCall(calls, i);
        return ask === null ? [] : [{ call, ask }];
      });
      const tools = calls.filter((_, i) => askOfCall(calls, i) === null);
      if (tools.length > 0) {
        const batch: BatchNode = {
          kind: "batch",
          key: `${entry.seq}:batch`,
          calls: tools.map((call) => {
            const node: CallNode = {
              kind: "call",
              key: call.id,
              callId: call.id,
              name: call.name,
              issued: entry,
              spot: null,
              result: null,
              telemetry: null,
            };
            byCall.set(call.id, node);
            return node;
          }),
        };
        nodes.push(batch);
      }
      // A question reads as a turn ending, after the step's other calls.
      for (const { call, ask } of asks) {
        askCalls.set(call.id, ask);
        nodes.push({
          kind: "leaf",
          key: `${entry.seq}:ask:${call.id}`,
          entry,
          ask,
        });
      }
      continue;
    }

    if ("ToolResult" in p) {
      const ask = askCalls.get(p.ToolResult.call_id);
      if (ask !== undefined) {
        ask.resolved = true;
        // A reply is the result of a submission, which is what carries a
        // `submission_id`; an interrupt's error result has none.
        nodes.push({
          kind: "leaf",
          key: `${entry.seq}:reply`,
          entry,
          reply: entry.submission_id === undefined ? "unanswered" : "answered",
        });
        continue;
      }
      const owner = byCall.get(p.ToolResult.call_id);
      if (owner !== undefined) {
        owner.result = entry;
        if (p.ToolResult.wire === "Pending") pending.set(entry.seq, owner);
      }
      continue;
    }

    // A verdict re-renders the slot of the result it names: the slot holds a
    // copy of that result's entry carrying the verdict's wire and line counts,
    // at the result's own seq (the address its images and whole content
    // resolve through). Never a leaf. One naming a result that was not
    // Pending has no slot in `pending` and is ignored.
    if ("ResultVerdict" in p) {
      const owner = pending.get(p.ResultVerdict.for_result);
      const held = owner?.result?.payload;
      if (
        owner !== undefined &&
        held !== undefined &&
        "ToolResult" in held &&
        p.ResultVerdict.wire !== "Pending"
      ) {
        const { distillate, wire } = p.ResultVerdict;
        const result = { ...held.ToolResult, wire, distillate };
        // A Pending result carries no counts of its own; absent stays absent.
        if (distillate === undefined) delete result.distillate;
        owner.result = { ...owner.result!, payload: { ToolResult: result } };
        pending.delete(p.ResultVerdict.for_result);
      }
      continue;
    }

    if ("Spot" in p) {
      const owner = byCall.get(p.Spot.call_id);
      if (owner !== undefined) owner.spot = p.Spot;
      continue;
    }

    if ("ToolTelemetry" in p) {
      const owner = byCall.get(p.ToolTelemetry.call_id);
      if (owner !== undefined) owner.telemetry = p.ToolTelemetry;
      continue;
    }

    // `Triage` is not rendered: it is per-result bookkeeping whose one visible
    // fact — how much of the output survived — the result header already
    // states as `kept N of M lines`. Rendering it as its own block put a
    // `keep: all` line after every result, which said nothing the reader did
    // not already have. Its genuinely batch-scoped fields (`inbox`, `retries`)
    // have no surface yet; when they get one it is the batch node's, not a
    // leaf's (see backlog/web/entry-architecture.md).
    if ("Triage" in p) continue;

    // A process lifecycle record (docs/supervisor.md) is not rendered either:
    // the transcript already carries both of its events in prose — the
    // backgrounding result and the exit notification — and this is the durable
    // record behind them, read by `GET /processes/:id`, not a block to show.
    if ("ProcessEvent" in p) continue;

    // The system prompt is audit data, not conversation: it is what the model
    // was told before the operator said anything, and a new session whose only
    // entry it is should read as empty rather than as a collapsed block.
    if ("Preamble" in p) continue;

    // A request stage (docs/log-and-projection.md) is telemetry behind the
    // `current_operations` topic, not something the session said or did.
    if ("RequestStage" in p) continue;

    // A push record (docs/web-push.md) is the notification trail, not
    // conversation.
    if ("Push" in p) continue;

    // A cold-check record (docs/control-api.md): the park, the person
    // action that released it, the resume. The `park` topic is the live state; this is the trail.
    if ("Cold" in p) continue;

    // A dispatch marker (Payload::CallStarted) appended before tool dispatch;
    // projects to nothing on the transcript.
    if ("CallStarted" in p) continue;

    // A title entry is appended even when the peek kept the previous text
    // (docs/session-title.md); only a change is something to read.
    if ("Title" in p) {
      if (p.Title.text === title) continue;
      title = p.Title.text;
    }

    nodes.push({ kind: "leaf", key: `${entry.seq}`, entry });
  }

  return nodes;
}

/** The call a pending gate names, if it is in the tree — the renderer uses this
 * to mark the call rather than searching the log a second time. */
export function callSeq(node: CallNode): Seq {
  return node.issued.seq;
}
