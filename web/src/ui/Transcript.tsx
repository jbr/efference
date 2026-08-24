// The transcript: the log projected into its hierarchy (`session/tree.ts`) and
// rendered whole, plus the pre-commit streaming tail and the pending-turn
// echoes. Structure lives in the projection; this file is the display half of
// it (docs/web-client.md — parity of information, no folding; the density
// toggles hide, they never drop).

import { memo, useMemo } from "react";
import type { ConfigEdit, ContentBlock, Mark, Spot } from "../api/types.ts";
import type { LiveBlock, PendingTurn, Streaming } from "../session/store.ts";
import type {
  BatchNode,
  CallNode,
  LeafNode,
  TranscriptNode,
} from "../session/tree.ts";
import type { CallId } from "../api/types.ts";
import { Markdown } from "./Markdown.tsx";
import { CallBlock, LiveCallBlock, ResultBlock } from "./ToolBlocks.tsx";
import { useFollowTail } from "./useFollowTail.ts";
import { useViewSettings } from "./viewSettings.ts";

// Blank-line joined: these bodies render as markdown, and distinct wire
// blocks are separate paragraphs — a single newline would soft-wrap the last
// line of one block into the first of the next.
const text = (blocks: ContentBlock[]) =>
  blocks
    .map((b) =>
      "Text" in b
        ? b.Text
        : "Thinking" in b
          ? b.Thinking.text
          : "[redacted thinking]",
    )
    .join("\n\n");

/** One transcript row: the label gutter and the content column. `label` is
 * `null` for the two voices marked structurally instead — a user turn carries
 * an accent gutter, an assistant turn is the unmarked default. Authorship is
 * something you see, not a noun you read (docs/tui.md); everything that is
 * not one of the two voices keeps its label, in the gutter, where the mock
 * puts it. The `you` label is a deliberate reversal of the first slice's
 * label-free user turn (docs/web-client.md). */
function Entry({
  label,
  as,
  children,
}: {
  label: string | null;
  as?: string;
  children: React.ReactNode;
}) {
  const classes = ["entry", as].filter((c) => c !== undefined).join(" ");
  return (
    <div className={classes}>
      <div className="label">{label}</div>
      <div className="content">{children}</div>
    </div>
  );
}

/** The spotter's account of a call: its own line inside the call, never part
 * of the call's content — a different rung's judgment *about* the call
 * (docs/tui.md). The stance leads, because that is the scannable part. A
 * clear verdict is the routine case, so it hides behind the density toggle;
 * anything that gated or watched always shows. Exported for the floating
 * gate panel (Gate.tsx), which restates the verdict while it is off-screen. */
export function SpotLine({ spot }: { spot: Spot }) {
  const { clearSpots } = useViewSettings();
  const clear = spot.stance === "clear" || spot.stance === "clear_always";
  if (clear && !clearSpots) return null;
  return (
    <div className="spot">
      <span className="spot-label">spot · {spot.stance}</span>
      <span className="spot-reason">{spot.reason}</span>
    </div>
  );
}

/** One line per config change, in the change's own vocabulary rather than its
 * JSON. The committed state is the unmarked case; proposed and cancelled say
 * so. */
function configLine(edit: ConfigEdit): string {
  const c = edit.change;
  const what =
    "command" in c
      ? `command · ${c.command.command}`
      : "path" in c
        ? `path · ${c.path.path} ${(c.path.read ? "r" : "") + (c.path.write ? "w" : "")}`
        : "tool" in c
          ? `tool · ${c.tool.name}`
          : `scope · ${c.scope.text}`;
  const line = edit.state === "committed" ? what : `${edit.state} · ${what}`;
  return edit.note !== "" ? `${line} — ${edit.note}` : line;
}

/** The consolidation mark (docs/consolidate.md): a boundary the wire folded
 * behind, rendered as a rule naming its extent, with the summary — the prose
 * that replaced the span — as the content. The count is entries, not the
 * mock's token delta: tokens are provider accounting the client does not
 * hold. Every entry it replaced is still on screen above (render-whole), so
 * there is nothing to expand — the marker tells the reader where the model's
 * context stops carrying the raw log. */
function Consolidation({
  mark,
  replaces,
}: {
  mark: Mark;
  replaces: number | undefined;
}) {
  if (!(typeof mark.kind === "object" && "Boundary" in mark.kind)) {
    // A mark kind this client does not know: the note still reads.
    return (
      <Entry label="mark">
        <Markdown>{mark.note}</Markdown>
      </Entry>
    );
  }
  const upto = mark.kind.Boundary.upto;
  return (
    <div className="consolidation">
      <div className="consolidation-rule">
        <span className="glyph">⊙</span>
        <span className="c-label">consolidated through #{upto}</span>
        {replaces !== undefined && (
          <span className="c-count">
            {replaces} {replaces === 1 ? "entry" : "entries"}
          </span>
        )}
      </div>
      <Entry label={null}>
        <Markdown>{mark.note}</Markdown>
      </Entry>
      <div className="consolidation-rule closing" />
    </div>
  );
}

/** One log entry rendered at session level. */
function Leaf({ node }: { node: LeafNode }) {
  const { entry } = node;
  const p = entry.payload;
  if ("User" in p) {
    return (
      <Entry label="you" as="user">
        <Markdown>{text(p.User)}</Markdown>
      </Entry>
    );
  }
  if ("Assistant" in p) {
    // Which half of the step this leaf stands for is in its key: the
    // projection emits thinking and prose as separate nodes.
    return node.key.endsWith(":thinking") ? (
      <Entry label="thinking" as="thinking">
        <Markdown>{text(p.Assistant.thinking)}</Markdown>
      </Entry>
    ) : (
      <Entry label={null}>
        <Markdown>{text(p.Assistant.text)}</Markdown>
      </Entry>
    );
  }
  if ("Mark" in p) {
    return <Consolidation mark={p.Mark} replaces={node.replaces} />;
  }
  if ("Ambient" in p) {
    return (
      <Entry label="ambient" as="ambient">
        <span className="ambient-source">{p.Ambient.source}</span> —{" "}
        {p.Ambient.body}
      </Entry>
    );
  }
  if ("ConfigEdit" in p) {
    return (
      <Entry label="config" as="config">
        {configLine(p.ConfigEdit)}
      </Entry>
    );
  }
  if ("Preamble" in p) {
    // Collapsed by default: this is presentation of one audit entry — the
    // wall of system prompt ahead of the first user turn — not a fold
    // affordance; render-whole for tool results stands (docs/web-client.md).
    return (
      <Entry label={null}>
        <details className="dim">
          <summary className="kind">preamble</summary>
          <pre>{p.Preamble.system}</pre>
        </details>
      </Entry>
    );
  }
  return (
    <Entry label="unknown entry" as="dim">
      <pre>{JSON.stringify(p)}</pre>
    </Entry>
  );
}

/** One call, whole: what was asked, what the harness made of it, what came
 * back. The container is what makes a result *belong to* its call rather than
 * merely follow it. */
const Call = memo(function Call({
  node,
  onInspect,
  onOpenFile,
}: {
  node: CallNode;
  onInspect: (callId: CallId) => void;
  onOpenFile: (path: string) => void;
}) {
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
  // The whole call opens the inspector — but never out from under a text
  // selection or a click that was for a control inside it.
  const open = (event: React.MouseEvent) => {
    if (window.getSelection()?.isCollapsed === false) return;
    const target = event.target as Element;
    if (target.closest("button, a, summary, input, select, textarea")) return;
    onInspect(node.callId);
  };
  // A resolved consolidate collapses to its header. Its input is the summary
  // the ⊙ mark below renders in full — the pair projects normally, so the
  // log holds the summary twice (docs/consolidate.md) — and its result is a
  // constant acknowledgement. A pending, gated, or failed consolidate
  // renders whole: the gate surface expects the call on screen, and an
  // error is information.
  if (node.name === "consolidate" && result !== null && !result.raw.is_error) {
    return (
      <div className="call clickable" onClick={open}>
        <div className="call-head">
          <span className="prompt">›</span>
          <span className="call-tool">consolidate</span>
        </div>
        {node.spot !== null && <SpotLine spot={node.spot} />}
      </div>
    );
  }
  return (
    <div className="call clickable" onClick={open}>
      {call !== null && <CallBlock call={call} onOpenFile={onOpenFile} />}
      {node.spot !== null && <SpotLine spot={node.spot} />}
      {node.result !== null && result !== null && (
        <ResultBlock
          tool={node.name}
          seq={node.result.seq}
          raw={result.raw}
          wire={result.wire}
        />
      )}
    </div>
  );
});

/** The calls of one step. A batch of one is still a batch structurally — the
 * node is what gives depth-based styling and per-batch state a home — but it
 * labels itself `tool call`: a batch of one flattens to a call semantically,
 * and `batch` appears only when there is a plurality to name. The header
 * states the count and nothing else: the mock's duration was invented, and
 * per-batch facts that arrive later (`inbox`, `retries`) belong on this row
 * when they get a surface. */
function Batch({
  node,
  onInspect,
  onOpenFile,
}: {
  node: BatchNode;
  onInspect: (callId: CallId) => void;
  onOpenFile: (path: string) => void;
}) {
  const n = node.calls.length;
  return (
    <div className="batch">
      <Entry label={n === 1 ? "tool call" : "batch"} as="batch-head">
        {n === 1 ? null : `${n} calls`}
      </Entry>
      <Entry label={null}>
        {node.calls.map((call) => (
          <Call
            key={call.key}
            node={call}
            onInspect={onInspect}
            onOpenFile={onOpenFile}
          />
        ))}
      </Entry>
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
    return (
      <Entry label="thinking…" as="thinking">
        <Markdown>{block.text}</Markdown>
      </Entry>
    );
  }
  if (block.kind === "text") {
    return (
      <Entry label={null}>
        <Markdown>{block.text}</Markdown>
      </Entry>
    );
  }
  return (
    <Entry label={null}>
      <LiveCallBlock name={block.name ?? ""} input={block.text} />
    </Entry>
  );
}

/** A glance at a running tool: the last lines of its live output, dimmed,
 * under a header stating how much there is. The one deliberate exception to
 * render-whole, and not a truncation — the whole output arrives, whole, when
 * the call commits (docs/tui.md states the same rule for the TUI's peek). */
const PEEK_LINES = 10;

function OutputPeek({ chunk }: { chunk: string }) {
  const lines = chunk.split("\n");
  const peeked = lines.length > PEEK_LINES;
  return (
    <Entry label="running…" as="peek">
      <div className="peek-head">
        ⋯ running · {lines.length} {lines.length === 1 ? "line" : "lines"}
        {peeked ? `, last ${PEEK_LINES}` : ""}
      </div>
      <pre className="dim">{lines.slice(-PEEK_LINES).join("\n")}</pre>
    </Entry>
  );
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
  const { thinking } = useViewSettings();
  const liveOutput = Object.entries(streaming.toolOutput);
  return (
    <>
      {streaming.blocks
        .filter((block) => thinking || block.kind !== "thinking")
        .map((block) => (
          <LiveBlockView key={block.index} block={block} />
        ))}
      {liveOutput.map(([callId, chunk]) => (
        <OutputPeek key={callId} chunk={chunk} />
      ))}
    </>
  );
}

/** The submitted turns the daemon accepted but has not put on the wire —
 * dimmed, `⋯`-marked echoes after the live tail, re-rendered as committed
 * turns when the matching `User` entry streams back (`session/store.ts`). */
function PendingTurns({ pending }: { pending: PendingTurn[] }) {
  return (
    <>
      {pending.map((turn) => (
        <Entry key={`pending:${turn.id}`} label="you" as="user pending">
          <Markdown>{turn.text}</Markdown>
          <div className="pending-mark">⋯ pending</div>
        </Entry>
      ))}
    </>
  );
}

export function Transcript({
  nodes,
  streaming,
  pending,
  onInspect,
  onOpenFile,
}: {
  /** The projected tree — built once in App, shared with the inspector. */
  nodes: TranscriptNode[];
  streaming: Streaming;
  pending: PendingTurn[];
  onInspect: (callId: CallId) => void;
  onOpenFile: (path: string) => void;
}) {
  useFollowTail();
  const { thinking, ambient } = useViewSettings();
  const shown = nodes.filter(
    (node) =>
      node.kind !== "leaf" ||
      ((thinking || !node.key.endsWith(":thinking")) &&
        (ambient || !("Ambient" in node.entry.payload))),
  );
  return (
    <div className="transcript col">
      {shown.map((node) =>
        node.kind === "batch" ? (
          <Batch
            key={node.key}
            node={node}
            onInspect={onInspect}
            onOpenFile={onOpenFile}
          />
        ) : (
          <MemoLeaf key={node.key} node={node} />
        ),
      )}
      <StreamingTail streaming={streaming} />
      <PendingTurns pending={pending} />
    </div>
  );
}
