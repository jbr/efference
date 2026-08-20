// Tool call and result blocks (docs/web-client.md): the display half over
// the cordon's parse (`render/parse.ts`). A call leads with its intent and
// summarizes its identifying argument; a result renders its tool-specific
// view — colored diff lines, framing-stripped file content, numbered kept
// lines — with the raw content one tap away whenever the body isn't
// verbatim raw. `data-lang` on code bodies is the future highlighter's hook.

import { useMemo } from "react";
import type { RawResult, ToolCall, WireState } from "../api/types.ts";
import { Code, NumberedCode, StreamingCode } from "./Code.tsx";
import {
  hasStructure,
  partialInput,
  streamingLang,
} from "../render/partial.ts";
import {
  callView,
  resultRender,
  wireLabel,
  type ResultView,
} from "../render/parse.ts";

/** The call's header row: the accent prompt, the tool, its identifying
 * argument. One shape for committed and streaming calls, so a call does not
 * change appearance at commit. */
function CallHead({
  name,
  summary,
  arriving,
}: {
  name: string;
  summary: string | null;
  arriving?: boolean;
}) {
  return (
    <div className="call-head">
      <span className="prompt">›</span>
      <span className="call-tool">{name}</span>
      {summary !== null && (
        <>
          <span className="call-sep">·</span>
          <span className="call-target">{summary}</span>
        </>
      )}
      {arriving === true && <span className="dim">…</span>}
    </div>
  );
}

export function CallBlock({ call }: { call: ToolCall }) {
  const view = useMemo(() => callView(call.name, call.input), [call]);
  return (
    <div className="block">
      <CallHead name={call.name} summary={view.summary} />
      {view.intent !== null && <div className="intent">{view.intent}</div>}
      {view.body !== null && (
        <Code text={view.body.text} lang={view.body.lang} />
      )}
    </div>
  );
}

/**
 * A call whose input is still arriving, rendered from the part that has —
 * the settled arguments as arguments, the one value in flight as content
 * (`render/partial.ts`, mirroring `src/tui/projection/nodes.rs`).
 *
 * The settled half goes through the same [`callView`] a committed call uses,
 * so the header fills in as bytes land rather than changing shape at commit.
 * The in-flight value never reaches it: what [`callView`] produces is an
 * identity claim (`write · src/api.rs`), and only a value whose closing quote
 * has arrived may back one. Before anything structural arrives, the raw
 * characters show — they claim nothing, which is what makes them safe.
 */
export function LiveCallBlock({
  name,
  input,
}: {
  name: string;
  input: string;
}) {
  const parsed = useMemo(() => partialInput(input), [input]);
  if (parsed === null || !hasStructure(parsed)) {
    return (
      <div className="block">
        <CallHead name={name} summary={null} arriving />
        <pre className="dim">{input}</pre>
      </div>
    );
  }
  const view = callView(name, parsed.settled);
  const { arriving } = parsed;
  return (
    <div className="block">
      <CallHead name={name} summary={view.summary} arriving />
      {view.intent !== null && <div className="intent">{view.intent}</div>}
      {view.body !== null && (
        <Code text={view.body.text} lang={view.body.lang} />
      )}
      {arriving !== null &&
        (arriving.text.includes("\n") ? (
          // A string with newlines is the case worth rendering as a block —
          // a file's contents, a heredoc — in the language of whatever
          // settled path names it, which is why the path arriving first is
          // what makes this worth doing.
          <StreamingCode
            text={arriving.text}
            lang={streamingLang(parsed.settled)}
          />
        ) : (
          <div className="dim">
            {arriving.key}: {arriving.text}…
          </div>
        ))}
    </div>
  );
}

/** How much of a file body the transcript shows; the inspector holds the
 * whole thing. Slack of 2 so the indicator never stands in for fewer lines
 * than it occupies. */
const TRUNCATE_AT = 12;

/** Truncate a read/write result's body for the transcript view. Render-whole
 * survives this: a gate decides a call, never a result, so no pending
 * decision rests on a truncated body — and the full body is one click away
 * (docs/web-client.md). */
function truncateBody(
  body: ResultView,
  tool: string | null,
): { body: ResultView; hidden: number } {
  if (tool !== "read" && tool !== "write") return { body, hidden: 0 };
  if (body.kind === "numbered" && body.lines.length > TRUNCATE_AT + 2) {
    return {
      body: { ...body, lines: body.lines.slice(0, TRUNCATE_AT) },
      hidden: body.lines.length - TRUNCATE_AT,
    };
  }
  if (body.kind === "code" || body.kind === "text") {
    const lines = body.text.split("\n");
    if (lines.length > TRUNCATE_AT + 2) {
      return {
        body: { ...body, text: lines.slice(0, TRUNCATE_AT).join("\n") },
        hidden: lines.length - TRUNCATE_AT,
      };
    }
  }
  return { body, hidden: 0 };
}

export function ResultBody({ body }: { body: ResultView }) {
  switch (body.kind) {
    case "diff":
      return (
        <pre>
          {body.lines.map((line, i) => (
            <div key={i} className={`diff-${line.kind}`}>
              {line.text}
            </div>
          ))}
        </pre>
      );
    case "code":
      return <Code text={body.text} lang={body.lang} />;
    case "numbered":
      return <NumberedCode lines={body.lines} lang={body.lang} />;
    case "text":
      return <pre>{body.text}</pre>;
  }
}

export function ResultBlock({
  tool,
  raw,
  wire,
}: {
  tool: string | null;
  raw: RawResult;
  wire: WireState;
}) {
  // The cordon parses the whole raw result — sentinel stripping, diff
  // classification, the kept-line filter. A store update re-renders every
  // entry, so without this a streamed token re-parses every result in the
  // session.
  const view = useMemo(() => resultRender(tool, raw, wire), [tool, raw, wire]);
  const { body, hidden } = useMemo(
    () => truncateBody(view.body, tool),
    [view, tool],
  );
  const label = [wireLabel(wire), view.sizeLabel, view.isError ? "error" : null]
    .filter((part) => part !== null)
    .join(", ");
  return (
    <div className="block">
      {/* No inspect link: the whole call is the inspection affordance, and
          the call's hover state is the discovery surface (Transcript.tsx). */}
      <div className="kind result-head">result ({label})</div>
      <ResultBody body={body} />
      {hidden > 0 && (
        <div className="more-lines dim">⋯ {hidden} more lines</div>
      )}
      {view.note !== null && (
        // The distiller's note sits beside the result as its own annotation
        // row — that rung's account *of* the output, not something the tool
        // printed (docs/tui.md renders it as an `Annotation` entry for the
        // same reason). The ✧ is display chrome; the wire's distillate keeps
        // its ⟢ (render/distill.ts).
        <div className="note">
          <span className="note-glyph">✧</span>
          <span className="note-text">{view.note}</span>
        </div>
      )}
      {/* The per-result raw <details> is gone: raw is the inspector's tab
          now, one click on the call away. */}
    </div>
  );
}
