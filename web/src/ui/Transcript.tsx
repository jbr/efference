// The transcript: every log entry, whole and in order, plus the pre-commit
// streaming tail (docs/web-client.md — parity of information, plain-text
// tool results first, no folding). Deliberately crude: this is the churn zone,
// and content-on-the-page precedes layout.

import type { ContentBlock, LogEntry, WireState } from "../api/types.ts";
import { wireView } from "../render/distill.ts";
import type { Streaming } from "../session/store.ts";
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

const wireLabel = (wire: WireState) => {
  if (wire === "Raw") return "raw";
  if (wire === "Elided") return "elided";
  if ("Distilled" in wire) return "distilled";
  return `triage failed: ${wire.TriageFailed.why}`;
};

function Entry({ entry }: { entry: LogEntry }) {
  const p = entry.payload;
  if ("User" in p) {
    return <Block kind="user" body={text(p.User)} />;
  }
  if ("Assistant" in p) {
    return (
      <>
        {p.Assistant.thinking.length > 0 && (
          <Block kind="thinking" body={text(p.Assistant.thinking)} dim />
        )}
        {p.Assistant.text.length > 0 && (
          <Block kind="assistant" body={text(p.Assistant.text)} />
        )}
        {p.Assistant.calls.map((call) => (
          <Block
            key={call.id}
            kind={`tool: ${call.name}`}
            body={JSON.stringify(call.input, null, 2)}
          />
        ))}
      </>
    );
  }
  if ("ToolResult" in p) {
    const { raw, wire } = p.ToolResult;
    const view = wireView(raw, wire);
    // The primary render is what the model saw (the distillate is the
    // output); `view` is null for a Raw wire state, where raw *is* that.
    // For every other state the raw content stays one tap away — the audit
    // view over the entry the client already holds.
    if (view === null) {
      return (
        <Block
          kind={`result${raw.is_error ? " (error)" : ""}`}
          body={raw.content}
        />
      );
    }
    return (
      <div className="block">
        <div className="kind">
          result ({wireLabel(wire)}
          {view.isError ? ", error" : ""})
        </div>
        <pre>{view.text}</pre>
        <details className="dim">
          <summary className="kind">raw</summary>
          <pre>{raw.content}</pre>
        </details>
      </div>
    );
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

function Block({
  kind,
  body,
  dim,
}: {
  kind: string;
  body: string;
  dim?: boolean;
}) {
  return (
    <div className={dim ? "block dim" : "block"}>
      <div className="kind">{kind}</div>
      <pre>{body}</pre>
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
        <Block kind="assistant…" body={streaming.assistant} />
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
  return (
    <div className="transcript">
      {entries.map((entry) => (
        <Entry key={entry.seq} entry={entry} />
      ))}
      <StreamingTail streaming={streaming} />
    </div>
  );
}
