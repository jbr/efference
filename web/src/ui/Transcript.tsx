// The transcript: the log projected into its hierarchy (`session/tree.ts`) and
// rendered whole, plus the pre-commit streaming tail. Structure lives in the
// projection; this file is the display half of it (docs/web-client.md — parity
// of information, no folding).

import { memo, useMemo } from "react";
import type { ContentBlock, LogEntry, Spot } from "../api/types.ts";
import type { LiveBlock, Streaming } from "../session/store.ts";
import {
  buildTree,
  type BatchNode,
  type CallNode,
  type LeafNode,
} from "../session/tree.ts";
import { Markdown } from "./Markdown.tsx";
import { CallBlock, LiveCallBlock, ResultBlock } from "./ToolBlocks.tsx";
import { useFollowTail } from "./useFollowTail.ts";

const text = (blocks: ContentBlock[]) =>
  blocks
    .map((b) =>
      "Text" in b
        ? b.Text
        : "Thinking" in b
          ? b.Thinking.text
          : "[redacted thinking]",
    )
    .join("\n");

/** `prose` renders the body as markdown; everything else is preformatted
 * text. The split is deliberate and narrow — see `Markdown.tsx`. Thinking in
 * particular stays preformatted: it is line-structured, not flowing prose,
 * and reflowing it collapses "- a\n- b" onto one line. */
function Block({
  kind,
  body,
  dim,
  prose,
  as,
}: {
  /** The label. `null` for the two voices that are marked structurally
   * instead: a user turn carries an accent gutter, an assistant turn is the
   * unmarked default. Authorship is something you see, not a noun you read
   * (docs/tui.md) — a label per entry is what made every entry look alike. */
  kind: string | null;
  body: string;
  dim?: boolean;
  prose?: boolean;
  as?: string;
}) {
  const classes = ["block", as, dim === true ? "dim" : null]
    .filter((c) => c !== undefined && c !== null)
    .join(" ");
  return (
    <div className={classes}>
      {kind !== null && <div className="kind">{kind}</div>}
      {prose ? <Markdown>{body}</Markdown> : <pre>{body}</pre>}
    </div>
  );
}

/** The spotter's account of a call: its own line inside the call, never part
 * of the call's content — a different rung's judgment *about* the call
 * (docs/tui.md). The stance leads, because that is the scannable part. */
function SpotLine({ spot }: { spot: Spot }) {
  return (
    <div className="dim spot">
      spotter {spot.stance} · {spot.reason}
    </div>
  );
}

/** One log entry rendered at session level. */
function Leaf({ node }: { node: LeafNode }) {
  const { entry } = node;
  const p = entry.payload;
  if ("User" in p) {
    return <Block kind={null} as="user" body={text(p.User)} prose />;
  }
  if ("Assistant" in p) {
    // Which half of the step this leaf stands for is in its key: the
    // projection emits thinking and prose as separate nodes.
    return node.key.endsWith(":thinking") ? (
      <Block kind="thinking" body={text(p.Assistant.thinking)} dim />
    ) : (
      <Block kind={null} body={text(p.Assistant.text)} prose />
    );
  }
  if ("Mark" in p) {
    return <Block kind="mark" body={p.Mark.note} dim />;
  }
  if ("Ambient" in p) {
    return (
      <Block kind={`ambient: ${p.Ambient.source}`} body={p.Ambient.body} dim />
    );
  }
  if ("ConfigEdit" in p) {
    return (
      <Block
        kind={`config ${p.ConfigEdit.state}`}
        body={JSON.stringify(p.ConfigEdit.change)}
        dim
      />
    );
  }
  if ("Preamble" in p) {
    // Collapsed by default: this is presentation of one audit entry — the
    // wall of system prompt ahead of the first user turn — not a fold
    // affordance; render-whole for tool results stands (docs/web-client.md).
    return (
      <details className="block dim">
        <summary className="kind">preamble</summary>
        <pre>{p.Preamble.system}</pre>
      </details>
    );
  }
  return <Block kind="unknown entry" body={JSON.stringify(p)} dim />;
}

/** One call, whole: what was asked, what the harness made of it, what came
 * back. The container is what makes a result *belong to* its call rather than
 * merely follow it. */
const Call = memo(function Call({ node }: { node: CallNode }) {
  const call = useMemo(() => {
    const issued = node.issued.payload;
    return "Assistant" in issued
      ? (issued.Assistant.calls.find((c) => c.id === node.callId) ?? null)
      : null;
  }, [node.issued, node.callId]);
  const result =
    node.result !== null && "ToolResult" in node.result.payload
      ? node.result.payload.ToolResult
      : null;
  return (
    <div className="call">
      {call !== null && <CallBlock call={call} />}
      {node.spot !== null && <SpotLine spot={node.spot} />}
      {result !== null && (
        <ResultBlock tool={node.name} raw={result.raw} wire={result.wire} />
      )}
    </div>
  );
});

/** The calls of one step. A batch of one is still a batch — the node is what
 * gives depth-based styling and per-batch state a home. */
function Batch({ node }: { node: BatchNode }) {
  return (
    <div className="batch">
      {node.calls.map((call) => (
        <Call key={call.key} node={call} />
      ))}
    </div>
  );
}

/** Memoized on its node: a committed node's rendering is a function of that
 * node alone, so it must not re-render because something else streamed. The
 * transcript re-renders on every delta, and re-rendering a committed entry
 * means re-parsing its markdown and re-tokenizing its code — enough, on a
 * session holding a few large results, to stop the page responding while a
 * turn streams. The TUI states the same rule for its own incremental
 * projection (docs/tui.md). `buildTree` rebuilds a call node when its result
 * or verdict lands, so the memo invalidates exactly then. */
const MemoLeaf = memo(Leaf);

function LiveBlockView({ block }: { block: LiveBlock }) {
  if (block.kind === "thinking") {
    return <Block kind="thinking…" body={block.text} dim />;
  }
  if (block.kind === "text") {
    return <Block kind="assistant…" body={block.text} prose />;
  }
  return <LiveCallBlock name={block.name ?? ""} input={block.text} />;
}

/** The pre-commit tail: the in-flight response's blocks, in wire order, each
 * identified by the `index` its deltas name (docs/control-api.md). A response
 * with two tool calls renders both — with one buffer per kind the second call's
 * start wiped the first, and the arguments already on screen disappeared.
 *
 * It renders flat rather than inside a batch: nothing here has committed, so
 * the calls among them have no results to contain and no verdicts to carry.
 * The committed entry supersedes the whole set at once. */
function StreamingTail({ streaming }: { streaming: Streaming }) {
  const liveOutput = Object.entries(streaming.toolOutput);
  return (
    <>
      {streaming.blocks.map((block) => (
        <LiveBlockView key={block.index} block={block} />
      ))}
      {liveOutput.map(([callId, chunk]) => (
        <Block key={callId} kind="output…" body={chunk} />
      ))}
    </>
  );
}

export function Transcript({
  entries,
  streaming,
}: {
  entries: LogEntry[];
  streaming: Streaming;
}) {
  useFollowTail();
  const nodes = useMemo(() => buildTree(entries), [entries]);
  return (
    <div className="transcript">
      {nodes.map((node) =>
        node.kind === "batch" ? (
          <Batch key={node.key} node={node} />
        ) : (
          <MemoLeaf key={node.key} node={node} />
        ),
      )}
      <StreamingTail streaming={streaming} />
    </div>
  );
}
