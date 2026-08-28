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

import type {
  CallId,
  LogEntry,
  Seq,
  Spot,
  ToolTelemetry,
} from "../api/types.ts";

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
  /** The `ToolResult` entry, once the call has returned. */
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
  const seqs: Seq[] = [];

  for (const entry of entries) {
    const p = entry.payload;
    seqs.push(entry.seq);

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
      const { thinking, text, calls } = p.Assistant;
      if (thinking.length > 0) {
        nodes.push({ kind: "leaf", key: `${entry.seq}:thinking`, entry });
      }
      if (text.length > 0) {
        nodes.push({ kind: "leaf", key: `${entry.seq}:text`, entry });
      }
      if (calls.length > 0) {
        const batch: BatchNode = {
          kind: "batch",
          key: `${entry.seq}:batch`,
          calls: calls.map((call) => {
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
      continue;
    }

    if ("ToolResult" in p) {
      const owner = byCall.get(p.ToolResult.call_id);
      if (owner !== undefined) owner.result = entry;
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

    nodes.push({ kind: "leaf", key: `${entry.seq}`, entry });
  }

  return nodes;
}

/** The call a pending gate names, if it is in the tree — the renderer uses this
 * to mark the call rather than searching the log a second time. */
export function callSeq(node: CallNode): Seq {
  return node.issued.seq;
}
