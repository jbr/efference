// The transcript: every log entry, whole and in order, plus the pre-commit
// streaming tail (docs/web-client.md — parity of information, plain-text
// tool results first, no folding). Deliberately crude: this is the churn zone,
// and content-on-the-page precedes layout.

import { memo, useMemo } from "react";
import type { CallId, ContentBlock, LogEntry } from "../api/types.ts";
import type { Streaming } from "../session/store.ts";
import { Markdown } from "./Markdown.tsx";
import { CallBlock, ResultBlock } from "./ToolBlocks.tsx";
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

/** Memoized on its entry: a committed entry's rendering is a function of that
 * entry alone, so it must not re-render because something else streamed. The
 * transcript re-renders on every delta, and re-rendering a committed entry
 * means re-parsing its markdown and re-tokenizing its code — which, on a
 * session holding a few large results, is enough to stop the page responding
 * while a turn streams. The TUI states the same rule for its own incremental
 * projection: a committed entry is re-projected only when a call it owns
 * changes (docs/tui.md). `toolOf` must therefore be stable, or this memo
 * never holds. */
const Entry = memo(function Entry({
  entry,
  toolOf,
}: {
  entry: LogEntry;
  toolOf: (callId: CallId) => string | null;
}) {
  const p = entry.payload;
  if ("User" in p) {
    return <Block kind="user" body={text(p.User)} prose />;
  }
  if ("Assistant" in p) {
    return (
      <>
        {p.Assistant.thinking.length > 0 && (
          <Block kind="thinking" body={text(p.Assistant.thinking)} dim />
        )}
        {p.Assistant.text.length > 0 && (
          <Block kind="assistant" body={text(p.Assistant.text)} prose />
        )}
        {p.Assistant.calls.map((call) => (
          <CallBlock key={call.id} call={call} />
        ))}
      </>
    );
  }
  if ("ToolResult" in p) {
    const { call_id, raw, wire } = p.ToolResult;
    return <ResultBlock tool={toolOf(call_id)} raw={raw} wire={wire} />;
  }
  if ("Mark" in p) {
    return <Block kind="mark" body={p.Mark.note} dim />;
  }
  if ("Triage" in p) {
    return <Block kind="triage" body={`keep: ${p.Triage.raw_keep}`} dim />;
  }
  if ("Ambient" in p) {
    return (
      <Block kind={`ambient: ${p.Ambient.source}`} body={p.Ambient.body} dim />
    );
  }
  if ("Spot" in p) {
    return <Block kind={`spot: ${p.Spot.stance}`} body={p.Spot.reason} dim />;
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
});

/** `prose` renders the body as markdown; everything else is preformatted
 * text. The split is deliberate and narrow — see `Markdown.tsx`. Thinking in
 * particular stays preformatted: it is line-structured, not flowing prose,
 * and reflowing it collapses "- a\n- b" onto one line. */
function Block({
  kind,
  body,
  dim,
  prose,
}: {
  kind: string;
  body: string;
  dim?: boolean;
  prose?: boolean;
}) {
  return (
    <div className={dim ? "block dim" : "block"}>
      <div className="kind">{kind}</div>
      {prose ? <Markdown>{body}</Markdown> : <pre>{body}</pre>}
    </div>
  );
}

/** The pre-commit tail: the in-flight response's blocks, in wire order, each
 * identified by the `index` its deltas name (docs/control-api.md). A response
 * with two tool calls renders both — with one buffer per kind the second call's
 * start wiped the first, and the arguments already on screen disappeared. */
function StreamingTail({ streaming }: { streaming: Streaming }) {
  const liveOutput = Object.entries(streaming.toolOutput);
  return (
    <>
      {streaming.blocks.map((block) => {
        if (block.kind === "thinking") {
          return (
            <Block key={block.index} kind="thinking…" body={block.text} dim />
          );
        }
        if (block.kind === "text") {
          return (
            <Block
              key={block.index}
              kind="assistant…"
              body={block.text}
              prose
            />
          );
        }
        return (
          <Block
            key={block.index}
            kind={`calling ${block.name ?? ""}…`}
            body={block.text}
          />
        );
      })}
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
  // A result names its call by id; the call (and its tool name) lives in an
  // earlier Assistant entry. Built once per entries change, not per render:
  // its identity is a dependency of every Entry's memo.
  const toolOf = useMemo(() => {
    const calls = new Map<CallId, string>();
    for (const entry of entries) {
      if ("Assistant" in entry.payload) {
        for (const call of entry.payload.Assistant.calls) {
          calls.set(call.id, call.name);
        }
      }
    }
    return (callId: CallId) => calls.get(callId) ?? null;
  }, [entries]);
  return (
    <div className="transcript">
      {entries.map((entry) => (
        <Entry key={entry.seq} entry={entry} toolOf={toolOf} />
      ))}
      <StreamingTail streaming={streaming} />
    </div>
  );
}
