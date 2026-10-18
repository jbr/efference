import { isHandoff } from "./inspector/handoff.ts";
import { Disclosure } from "./Disclosure.tsx";
// The transcript: the log projected into its hierarchy (`session/tree.ts`),
// rendered as items at a level of detail set by their distance from the tail
// (`ui/TranscriptItems.tsx`, rules in `ui/transcriptLevels.ts`), plus the
// pre-commit streaming tail and the pending-turn echoes. Nothing is dropped:
// every folded item opens whole on a tap, and the density toggles hide, they
// never drop (docs/web-client.md).

import { handoffPredecessor, senderName } from "../api/types.ts";
import { memo, useMemo, type ComponentProps } from "react";
import type {
  Ask,
  Attachment,
  ConfigEdit,
  Summary,
  Spot,
  UserBlock,
  WorktreeEvent,
} from "../api/types.ts";
import type { LiveBlock, PendingTurn, Streaming } from "../session/store.ts";
import type { CallNode, LeafNode, TranscriptNode } from "../session/tree.ts";
import { upstreamFailureWhy } from "../session/tree.ts";
import { assistantView } from "../session/assistant.ts";
import type { CallId } from "../api/types.ts";
import { askMarkText, askStanding } from "./askMark.ts";
import { Markdown } from "./Markdown.tsx";
import { Ambient } from "./Ambient.tsx";
import { resumedLine } from "./resumed.ts";
import { cleanWorktreeOutput, setupNote } from "./worktree.ts";
import { SentAttachments } from "./Attachments.tsx";
import { CallBlock, LiveCallBlock, ResultBlock } from "./ToolBlocks.tsx";
import { useFollowTail } from "./useFollowTail.ts";
import { useViewSettings } from "./viewSettings.ts";
import {
  LiveResult,
  LiveThinking,
  TranscriptItems,
} from "./TranscriptItems.tsx";

// Blank-line joined: these bodies render as markdown, and distinct wire
// blocks are separate paragraphs — a single newline would soft-wrap the last
// line of one block into the first of the next.
const text = (blocks: UserBlock[]) =>
  blocks
    .filter((b) => !("Attachment" in b))
    .map((b) =>
      "Text" in b
        ? b.Text
        : "Thinking" in b
          ? b.Thinking.text
          : "[redacted thinking]",
    )
    .join("\n\n");

const attachmentsOf = (blocks: UserBlock[]): Attachment[] =>
  blocks.flatMap((b) => ("Attachment" in b ? [b.Attachment] : []));

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
  title,
  children,
}: {
  label: string | null;
  as?: string;
  /** The label's tooltip: a sourced turn's whole `source`. */
  title?: string;
  children: React.ReactNode;
}) {
  const classes = ["entry", as].filter((c) => c !== undefined).join(" ");
  return (
    <div className={classes}>
      <div className="label" title={title}>
        {label}
      </div>
      <div className="content">{children}</div>
    </div>
  );
}

/** A user turn, or an `ask` reply, under its sender: `you` for the operator, the
 * agent's id or the client's name for a turn another agent sent, with the
 * whole `source` as the label's tooltip (docs/web-client.md). */
function UserEntry({
  source,
  children,
}: {
  source: string | null | undefined;
  children: React.ReactNode;
}) {
  const name = senderName(source);
  return (
    <Entry
      label={name}
      as={name === "you" ? "user" : "user agent"}
      title={name === "you" ? undefined : (source ?? undefined)}
    >
      {children}
    </Entry>
  );
}

/** A worktree event as a notice, in the shape of a `Resumed` one: one line
 * naming what happened, with git's setup output — which carries the project's
 * `post-checkout` hook output — collapsed behind a disclosure. The branch
 * renders as a code chip, as identifiers do elsewhere in the UI. */
function WorktreeNotice({ event }: { event: WorktreeEvent }) {
  const setup =
    event.kind.event === "added" || event.kind.event === "restored"
      ? cleanWorktreeOutput(event.kind.output)
      : "";
  return (
    <Entry label="worktree" as="ambient">
      <WorktreeLine event={event} />
      {setup !== "" && (
        <Disclosure className="worktree-setup" label="Show setup output">
          <pre>{setup}</pre>
        </Disclosure>
      )}
    </Entry>
  );
}

function WorktreeLine({ event }: { event: WorktreeEvent }) {
  switch (event.kind.event) {
    case "added":
      return (
        <>
          worktree added on{" "}
          <code className="worktree-branch">{event.branch}</code>
          {setupNote(event.kind.exit_code)}
        </>
      );
    case "restored":
      return (
        <>
          worktree restored from{" "}
          <code className="worktree-branch">{event.branch}</code>
          {setupNote(event.kind.exit_code)}
        </>
      );
    case "removed":
      return event.kind.branch_deleted
        ? "worktree removed (branch deleted)"
        : "worktree removed (branch kept)";
    case "kept":
      return `worktree kept: ${event.kind.reason}`;
    case "remove_failed":
      return `worktree removal failed: ${event.kind.error}`;
  }
}

/** Why a call waiting at the gate was held: the spotter's reason, as its own
 * line inside the call — a different rung's judgment *about* the call
 * (docs/tui.md). It stands alone, without a label: the gated entry and its
 * controls already say what is being asked. Only a waiting call draws it; once
 * decided, the outcome is the call's result and the verdict is the
 * inspector's. Exported for the floating gate panel (Gate.tsx), which
 * restates it while the call is off-screen. */
export function SpotLine({ spot }: { spot: Spot }) {
  return <p className="spot">{sentence(spot.reason)}</p>;
}

/** The spotter writes its reason as a clause; standing alone it opens a
 * sentence. */
const sentence = (text: string) => text.charAt(0).toUpperCase() + text.slice(1);

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

/** A summary (docs/summary.md): a range the wire replaced, rendered as a rule
 * naming its extent, with the summary text — the prose that replaced the
 * range — as the content. The count is entries, not the
 * mock's token delta: tokens are provider accounting the client does not
 * hold. Every entry it replaced is still on screen above (render-whole), so
 * there is nothing to expand — the rule tells the reader where the model's
 * context stops carrying the raw log. */
function SummaryRule({
  summary,
  replaces,
}: {
  summary: Summary;
  replaces: number | undefined;
}) {
  const verb = summary.made_by === undefined ? "consolidated" : "compacted";
  const label =
    summary.from === 0
      ? `${verb} through #${summary.upto}`
      : `${verb} #${summary.from}–#${summary.upto}`;
  return (
    <div className="summary">
      <div className="summary-rule">
        <span className="glyph">⊙</span>
        <span className="c-label">{label}</span>
        {replaces !== undefined && (
          <span className="c-count">
            {replaces} {replaces === 1 ? "entry" : "entries"}
          </span>
        )}
      </div>
      <Entry label={null}>
        <Markdown>{summary.text}</Markdown>
      </Entry>
      <div className="summary-rule closing" />
    </div>
  );
}

/** One log entry rendered at session level. */
function Leaf({
  node,
  liveAsk,
}: {
  node: LeafNode;
  /** The `ask` topic, for ask leaves only (others get `null` so a topic change
   * does not re-render them). */
  liveAsk: Ask | null;
}) {
  const { entry } = node;
  const p = entry.payload;
  // An `ask` reads as a turn ending: the question is the assistant's prose
  // with a small mark, and the reply the operator's message (docs/web-client.md).
  if (node.ask !== undefined) {
    const standing = askStanding(node.ask, liveAsk);
    return (
      <Entry label={null} as={`ask-question ask-question-${standing}`}>
        <p className={`ask-mark ask-${standing}`}>
          {askMarkText(node.ask, standing)}
        </p>
        <Markdown>{node.ask.question}</Markdown>
      </Entry>
    );
  }
  if (node.reply !== undefined && "ToolResult" in p) {
    if (node.reply === "unanswered") {
      return (
        <Entry label={null}>
          <p className="ask-mark">not answered</p>
        </Entry>
      );
    }
    return (
      <UserEntry source={entry.source}>
        <Markdown>{p.ToolResult.raw.content}</Markdown>
      </UserEntry>
    );
  }
  if ("User" in p) {
    // A hand-off brief is the predecessor speaking (docs/tools.md): shown as
    // the harness's note of the hand-off, never as the operator's turn.
    const predecessor = handoffPredecessor(entry.source);
    if (predecessor !== null) {
      return (
        <Entry label="handed off" as="ambient">
          <span className="ambient-source">from {predecessor}</span> — its
          brief:
          <Markdown>{text(p.User)}</Markdown>
        </Entry>
      );
    }
    return (
      <UserEntry source={entry.source}>
        <Markdown>{text(p.User)}</Markdown>
        <SentAttachments attachments={attachmentsOf(p.User)} />
      </UserEntry>
    );
  }
  if ("Assistant" in p) {
    // Which half of the step this leaf stands for is in its key: the
    // projection emits the record-only marker, thinking, and prose as
    // separate nodes.
    if (node.key.endsWith(":aborted")) {
      const record = p.Assistant.upstream_failure;
      return (
        <Entry label={null}>
          <div className="record-only">
            {record !== undefined &&
            upstreamFailureWhy(record) === "generation interrupted"
              ? "Generation interrupted — recorded, not in context"
              : "Incomplete response — recorded, not in context"}
          </div>
        </Entry>
      );
    }
    // Thinking leaves are TranscriptItems' own (they fold by level); what
    // reaches here is the step's prose.
    const view = assistantView(p.Assistant);
    return (
      <Entry label={null}>
        <Markdown>{text(view.text)}</Markdown>
      </Entry>
    );
  }
  if ("Summary" in p) {
    return <SummaryRule summary={p.Summary} replaces={node.replaces} />;
  }
  if ("Ambient" in p) {
    return (
      <Entry label="ambient" as="ambient">
        <Ambient source={p.Ambient.source} body={p.Ambient.body} />
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
  if ("Title" in p) {
    return (
      <Entry label={null}>
        <p className="title-change">
          Title updated to{" "}
          <span className="title-change-text">{p.Title.text}</span>
        </p>
      </Entry>
    );
  }
  if ("Resumed" in p) {
    return (
      <Entry label="resumed" as="ambient">
        {resumedLine(p.Resumed, node.entry.seq)}
      </Entry>
    );
  }
  if ("Worktree" in p) {
    return <WorktreeNotice event={p.Worktree} />;
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
  liveOutput,
  gateSlot,
  onInspect,
  onOpenFile,
}: {
  node: CallNode;
  /** Set while this call is held at the approval gate: the element the gate
   * surface renders its decision into (ui/Gate.tsx), under the verdict. */
  gateSlot?: (element: HTMLDivElement | null) => void;
  /** The running command's output so far, while its result has not landed —
   * shown in the slot the result will take (ui/TranscriptItems.tsx LiveResult). */
  liveOutput?: string;
  onInspect: (callId: CallId) => void;
  onOpenFile: (path: string, callId: CallId) => void;
}) {
  const call = useMemo(() => {
    const issued = node.issued.payload;
    return "Assistant" in issued
      ? (assistantView(issued.Assistant).calls.find(
          (c) => c.id === node.callId,
        ) ?? null)
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
    if (target.closest(".gate-slot")) return;
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
      </div>
    );
  }
  return (
    <div className="call clickable" onClick={open}>
      {call !== null && <CallBlock call={call} onOpenFile={onOpenFile} />}
      {gateSlot !== undefined && node.spot !== null && (
        <SpotLine spot={node.spot} />
      )}
      {gateSlot !== undefined && <div className="gate-slot" ref={gateSlot} />}
      {node.result === null && liveOutput !== undefined && (
        <LiveResult text={liveOutput} />
      )}
      {node.result !== null &&
        result !== null &&
        (isHandoff(result.raw.content, node.telemetry?.bash?.handle) ? (
          <div
            className="dim"
            style={{ fontSize: ".8125rem", padding: ".4rem 0" }}
          >
            Backgrounded · {node.telemetry?.bash?.handle}
          </div>
        ) : (
          <ResultBlock
            tool={node.name}
            seq={node.result.seq}
            raw={result.raw}
            wire={result.wire}
            selection={result.selection}
          />
        ))}
    </div>
  );
});

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
  if (block.kind === "thinking") return <LiveThinking text={block.text} />;
  if (block.kind === "text") {
    return (
      <Entry label={null}>
        <Markdown>{block.text}</Markdown>
      </Entry>
    );
  }
  // An `ask` is drawn as prose once committed; arriving as a call it would
  // show the plumbing.
  if (block.name === "ask") return null;
  // The same item a committed call is (TranscriptItems), so nothing moves
  // when it commits.
  return (
    <div className="t-entry t-call">
      <div className="call">
        <LiveCallBlock name={block.name ?? ""} input={block.text} />
      </div>
    </div>
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
function StreamingTail({
  streaming,
  committed,
}: {
  streaming: Streaming;
  /** Calls already in the tree: their output renders in their own result
   * slot, so only output for a call not yet committed peeks here. */
  committed: Set<CallId>;
}) {
  const { thinking } = useViewSettings();
  const liveOutput = Object.entries(streaming.toolOutput).filter(
    ([callId]) => !committed.has(callId),
  );
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
function PendingTurns({
  pending,
  answering,
  onRecall,
}: {
  pending: PendingTurn[];
  /** An `ask` is pending: what is sent now is its reply, which is never
   * queued, so there is nothing to recall and the agent is not "reading it at
   * its next step" (docs/web-client.md). */
  answering: boolean;
  onRecall: (id: string) => void;
}) {
  return (
    <>
      {pending.map((turn) => (
        <Entry key={`pending:${turn.id}`} label="you" as="user pending">
          <Markdown>{turn.text}</Markdown>
          <SentAttachments attachments={turn.attachments} />
          {/* A state, then the one thing to do about it — two pieces, not
              one run of text. */}
          <div className="pending-mark">
            <span>
              {answering
                ? "Sent · answering the question"
                : "Queued · the agent reads it at its next step"}
            </span>
            {/* Recall is a suffix pop: this turn and every one queued after it
             * come back to the composer together (docs/web-client.md). */}
            {!answering && (
              <button
                type="button"
                className="pending-recall"
                title="Pull this turn (and any queued after it) back into the composer"
                onClick={() => onRecall(turn.id)}
              >
                Recall
              </button>
            )}
          </div>
        </Entry>
      ))}
    </>
  );
}

export function TranscriptContent({
  nodes,
  streaming,
  pending,
  ask,
  gatedCall,
  onGateSlot,
  onRecall,
  onInspect,
  onOpenFile,
}: {
  /** The projected tree — built once in App, shared with the inspector. */
  nodes: TranscriptNode[];
  /** The call the approval gate holds, if any, and where its slot element
   * goes once mounted (ui/Gate.tsx). */
  gatedCall: CallId | null;
  onGateSlot: (element: HTMLDivElement | null) => void;
  streaming: Streaming;
  pending: PendingTurn[];
  /** The `ask` topic: while non-null, a send answers the current question. */
  ask: Ask | null;
  onRecall: (id: string) => void;
  onInspect: (callId: CallId) => void;
  onOpenFile: (path: string, callId: CallId) => void;
}) {
  const { thinking, ambient } = useViewSettings();
  const shown = useMemo(
    () =>
      ambient
        ? nodes
        : nodes.filter(
            (node) =>
              node.kind !== "leaf" || !("Ambient" in node.entry.payload),
          ),
    [nodes, ambient],
  );
  const committed = useMemo(
    () =>
      new Set(
        nodes.flatMap((node) =>
          node.kind === "batch" ? node.calls.map((call) => call.callId) : [],
        ),
      ),
    [nodes],
  );
  return (
    <div className="transcript col">
      <TranscriptItems
        nodes={shown}
        thinking={thinking}
        gatedCall={gatedCall}
        renderLeaf={(node) => (
          <MemoLeaf node={node} liveAsk={node.ask === undefined ? null : ask} />
        )}
        renderFullCall={(node) => (
          <Call
            node={node}
            liveOutput={streaming.toolOutput[node.callId]}
            gateSlot={node.callId === gatedCall ? onGateSlot : undefined}
            onInspect={onInspect}
            onOpenFile={onOpenFile}
          />
        )}
      />
      <StreamingTail streaming={streaming} committed={committed} />
      <PendingTurns
        pending={pending}
        answering={ask !== null}
        onRecall={onRecall}
      />
    </div>
  );
}

/** The session view follows the window tail; embedded specimens only render content (docs/web-client.md). */
export function Transcript(props: ComponentProps<typeof TranscriptContent>) {
  useFollowTail();
  return <TranscriptContent {...props} />;
}
