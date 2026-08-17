// The transcript: every log entry, whole and in order, plus the pre-commit
// streaming tail (docs/web-client.md — parity of information, plain-text
// tool results first, no folding). Deliberately crude: this is the churn zone,
// and content-on-the-page precedes layout.

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

function Entry({
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
}

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

/** The lossy pre-commit tail: the coarse phase a turn is in, live. */
function StreamingTail({ streaming }: { streaming: Streaming }) {
  const liveOutput = Object.entries(streaming.toolOutput);
  return (
    <>
      {streaming.thinking && (
        <Block kind="thinking…" body={streaming.thinking} dim />
      )}
      {streaming.assistant && (
        <Block kind="assistant…" body={streaming.assistant} prose />
      )}
      {streaming.toolName && (
        <Block
          kind={`calling ${streaming.toolName}…`}
          body={streaming.toolInput}
        />
      )}
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
  // earlier Assistant entry. One pass per render keeps Entry lookups O(1).
  const calls = new Map<CallId, string>();
  for (const entry of entries) {
    if ("Assistant" in entry.payload) {
      for (const call of entry.payload.Assistant.calls) {
        calls.set(call.id, call.name);
      }
    }
  }
  const toolOf = (callId: CallId) => calls.get(callId) ?? null;
  return (
    <div className="transcript">
      {entries.map((entry) => (
        <Entry key={entry.seq} entry={entry} toolOf={toolOf} />
      ))}
      <StreamingTail streaming={streaming} />
    </div>
  );
}
