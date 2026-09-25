// The transcript's items at their levels (docs/web-client.md, "Detail by
// recency"; the rules are ui/transcriptLevels.ts): a call whole, capped under
// a fade, or as a one-line row; thinking whole, previewed, or as a pill; and
// the live thinking tail, which scrolls inside itself. User turns and
// assistant prose always render whole, through the ordinary leaf renderer.

import { useEffect, useMemo, useRef, useState, type ReactNode } from "react";
import type { CallId, ContentBlock } from "../api/types.ts";
import { assistantView } from "../session/assistant.ts";
import type { CallNode, LeafNode, TranscriptNode } from "../session/tree.ts";
import { callView, resultRender } from "../render/parse.ts";
import { InlineMarkdown, Markdown } from "./Markdown.tsx";
import {
  flatten,
  isThinking,
  type Level,
  levelFor,
  THINKING_FLOOR,
  wordCount,
} from "./transcriptLevels.ts";
import "./Transcript.css";

const joinText = (blocks: ContentBlock[]) =>
  blocks
    .map((b) =>
      "Text" in b
        ? b.Text
        : "Thinking" in b
          ? b.Thinking.text
          : "[redacted thinking]",
    )
    .join("\n\n");

/** A box of fixed height pinned to its newest line: a long body scrolls
 * inside itself instead of pushing the page. The top fades only while lines
 * are scrolled out above it; a body that never outgrows the box has nothing
 * hidden and no fade. `pinned` is false once the body stops growing — the box
 * then holds where the reader left it. The live thinking tail, a committed
 * long think in the recent tier, and a running command's output are all this
 * box, so an item that finishes streaming does not change shape. */
export function PinnedBox({
  children,
  grows,
  className,
}: {
  children: ReactNode;
  /** Changes whenever the body grows; the box re-pins to the bottom. */
  grows: unknown;
  className?: string;
}) {
  const box = useRef<HTMLDivElement>(null);
  const [clipped, setClipped] = useState(false);
  useEffect(() => {
    const element = box.current;
    if (!element) return;
    element.scrollTop = element.scrollHeight;
    setClipped(element.scrollTop > 0);
  }, [grows]);
  return (
    <div
      ref={box}
      className={["t-pinned", clipped && "clipped", className]
        .filter(Boolean)
        .join(" ")}
      onScroll={(event) => setClipped(event.currentTarget.scrollTop > 0)}
    >
      {children}
    </div>
  );
}

/** A thinking block at a level. Short blocks ignore the level. A long one in
 * the recent tier stays in the box it streamed in, stopped at its last line,
 * so committing it moves nothing; older tiers preview it or fold it to a
 * pill. */
function Thinking({ text, level }: { text: string; level: Level }) {
  const [open, setOpen] = useState(false);
  const words = wordCount(text);
  const effective = open || words <= THINKING_FLOOR ? "whole" : level;
  if (effective === "brief") {
    return (
      <button className="t-fold" onClick={() => setOpen(true)}>
        Thought · {words} words
      </button>
    );
  }
  if (effective === "trimmed") {
    return (
      <Capped
        short
        label={`Show all ${words} words`}
        onExpand={() => setOpen(true)}
      >
        <div className="t-thinking">
          <Markdown>{text}</Markdown>
        </div>
      </Capped>
    );
  }
  if (effective === "full") {
    return (
      <>
        <div className="t-box-head">
          Thought · {words} words ·{" "}
          <button className="t-box-open" onClick={() => setOpen(true)}>
            show all
          </button>
        </div>
        <PinnedBox grows={text} className="t-thinking">
          <Markdown>{text}</Markdown>
        </PinnedBox>
      </>
    );
  }
  return (
    <div className="t-thinking">
      <Markdown>{text}</Markdown>
    </div>
  );
}

/** The live thinking tail, in the box it will stay in once committed. */
export function LiveThinking({ text }: { text: string }) {
  return (
    <div className="t-entry">
      <div className="t-box-head">Thinking…</div>
      <PinnedBox grows={text} className="t-thinking">
        <Markdown>{text}</Markdown>
      </PinnedBox>
    </div>
  );
}

/** A running command's output, in its call's result slot: the tail in the
 * same pinned box, under a header counting lines. The triaged result replaces
 * it when it lands. */
export function LiveResult({ text }: { text: string }) {
  const lines = text.split("\n").length;
  return (
    <div className="block t-live-result">
      <div className="kind result-head">
        running · {lines} {lines === 1 ? "line" : "lines"}
      </div>
      <PinnedBox grows={text} className="t-output">
        <pre>{text}</pre>
      </PinnedBox>
    </div>
  );
}

/** The call's head in words: its tool, then what it acted on. */
function ActionHead({ node }: { node: CallNode }) {
  const view = useMemo(() => {
    const issued = node.issued.payload;
    const call =
      "Assistant" in issued
        ? assistantView(issued.Assistant).calls.find(
            (c) => c.id === node.callId,
          )
        : undefined;
    return call ? callView(call.name, call.input) : null;
  }, [node]);
  if (view === null) return <span className="t-tool">{node.name}</span>;
  const target =
    view.target ??
    view.args
      // What it acted on is its string arguments; flags and counts are how.
      ?.filter((arg) => arg.key !== "intent" && /^["[]/.test(arg.text))
      .map((arg) => arg.text.replace(/^\[|\]$/g, "").replace(/"/g, ""))
      .join(" · ") ??
    "";
  // The intent is the row's title, as it is a full call's; what the call
  // was comes under it, smaller.
  return (
    <>
      {view.intent !== null && (
        <span className="t-intent">
          <InlineMarkdown>{view.intent}</InlineMarkdown>
        </span>
      )}
      <span className="t-tool">
        {view.server !== null && `${view.server}.`}
        {view.tool}
      </span>
      <span className="t-target">{target}</span>
    </>
  );
}

function noteOf(node: CallNode): string | null {
  const payload = node.result?.payload;
  if (!payload || !("ToolResult" in payload)) return null;
  const { raw, wire, distillate } = payload.ToolResult;
  return resultRender(node.name, raw, wire, distillate).note;
}

function Note({ node }: { node: CallNode }) {
  const note = noteOf(node);
  return note === null ? null : (
    <div className="t-note">
      <InlineMarkdown>{note}</InlineMarkdown>
    </div>
  );
}

/** Content held to a fixed height. Only when it actually overflows does it
 * fade out and carry the expand button, centred over the fade. */
function Capped({
  children,
  label,
  short = false,
  onExpand,
}: {
  children: ReactNode;
  label: string;
  /** A few lines rather than a screenful: the thinking preview. */
  short?: boolean;
  onExpand: () => void;
}) {
  const clip = useRef<HTMLDivElement>(null);
  const [overflowing, setOverflowing] = useState(false);
  useEffect(() => {
    const element = clip.current;
    if (!element) return;
    const measure = () =>
      setOverflowing(element.scrollHeight > element.clientHeight + 1);
    const observer = new ResizeObserver(measure);
    observer.observe(element);
    for (const child of element.children) observer.observe(child);
    return () => observer.disconnect();
  }, []);
  return (
    <div
      className={["t-cap", short && "short", overflowing && "overflowing"]
        .filter(Boolean)
        .join(" ")}
    >
      <div ref={clip} className="t-cap-clip">
        {children}
      </div>
      {overflowing && (
        <button className="t-more" onClick={onExpand}>
          {label}
        </button>
      )}
    </div>
  );
}

/** A call at a level. `renderFull` is the current full call block. */
function CallItem({
  node,
  level,
  gated,
  renderFull,
}: {
  node: CallNode;
  level: Level;
  /** Held at the approval gate: rendered whole whatever its level, marked,
   * with the decision inside it (ui/Gate.tsx). */
  gated: boolean;
  renderFull: (node: CallNode) => ReactNode;
}) {
  const [open, setOpen] = useState(false);
  if (gated) {
    return <div className="t-entry t-call gated">{renderFull(node)}</div>;
  }
  if (open || level === "full") {
    return <div className="t-entry t-call">{renderFull(node)}</div>;
  }
  if (level === "brief") {
    return (
      <button className="t-entry t-row" onClick={() => setOpen(true)}>
        <ActionHead node={node} />
        <Note node={node} />
      </button>
    );
  }
  // Trimmed: the whole call, its result capped to a few lines under a fade,
  // and the note lifted out from under the fade to below it.
  return (
    <div className="t-entry t-call t-capped">
      <Capped label="Show the whole result" onExpand={() => setOpen(true)}>
        {renderFull(node)}
      </Capped>
      <Note node={node} />
    </div>
  );
}

export function TranscriptItems({
  nodes,
  thinking,
  gatedCall,
  renderLeaf,
  renderFullCall,
}: {
  nodes: TranscriptNode[];
  /** The density switch: whether thinking shows at all. */
  thinking: boolean;
  /** The call the approval gate holds, if any. */
  gatedCall: CallId | null;
  renderLeaf: (node: LeafNode) => ReactNode;
  renderFullCall: (node: CallNode) => ReactNode;
}) {
  const items = useMemo(
    () =>
      flatten(nodes).filter(
        (item) => thinking || item.kind !== "leaf" || !isThinking(item.node),
      ),
    [nodes, thinking],
  );
  return (
    <>
      {items.map((item, i) => {
        const level = levelFor(item, items.length - 1 - i);
        if (item.kind === "call") {
          return (
            <CallItem
              key={item.node.key}
              node={item.node}
              level={level}
              gated={item.node.callId === gatedCall}
              renderFull={renderFullCall}
            />
          );
        }
        const payload = item.node.entry.payload;
        if (isThinking(item.node) && "Assistant" in payload) {
          return (
            <div key={item.node.key} className="t-entry">
              <Thinking
                text={joinText(assistantView(payload.Assistant).thinking)}
                level={level}
              />
            </div>
          );
        }
        return <div key={item.node.key}>{renderLeaf(item.node)}</div>;
      })}
    </>
  );
}
