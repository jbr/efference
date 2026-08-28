// Tool call and result blocks (docs/web-client.md): the display half over
// the cordon's parse (`render/parse.ts`). A call leads with its intent and
// summarizes its identifying argument; a result renders its tool-specific
// view — colored diff lines, framing-stripped file content, numbered kept
// lines — with the raw content one tap away whenever the body isn't
// verbatim raw. `data-lang` on code bodies is the future highlighter's hook.

import { useMemo } from "react";
import type { CallId, RawResult, ToolCall, WireState } from "../api/types.ts";
import { Code, NumberedCode, StreamingCode } from "./Code.tsx";
import {
  hasStructure,
  partialInput,
  streamingLang,
} from "../render/partial.ts";
import {
  byteSize,
  callView,
  resultRender,
  type ResultView,
} from "../render/parse.ts";
import { openablePaths } from "../session/touchedFiles.ts";
import { useNarrow } from "./useNarrow.ts";
import { useWholeResult } from "./wholeResult.ts";

/** The call's header row: the accent prompt, the tool, its identifying
 * argument. One shape for committed and streaming calls, so a call does not
 * change appearance at commit. */
function CallHead({
  name,
  summary,
  arriving,
  trailing,
}: {
  name: string;
  summary: string | null;
  arriving?: boolean;
  trailing?: React.ReactNode;
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
      {trailing}
    </div>
  );
}

/** The `open ⤢` links a call carries: one per viewable file it names —
 * within-cwd only (session/touchedFiles.ts) — opening the *current* file
 * from disk, never the file at the time of the call. Buttons, so the call's
 * open-the-inspector click handler ignores them (Transcript.tsx). */
export function OpenFileLinks({
  paths,
  callId,
  onOpenFile,
}: {
  paths: string[];
  /** The call the links belong to: an edit's own result is where the viewer
   * gets the lines to land on (session/editAnchor.ts). */
  callId: CallId;
  onOpenFile: (path: string, callId: CallId) => void;
}) {
  if (paths.length === 0) return null;
  return (
    <span className="open-file-links">
      {paths.map((path) => (
        <button
          key={path}
          className="linkish open-file"
          title={`view ${path} as it is now on disk`}
          onClick={() => onOpenFile(path, callId)}
        >
          {paths.length === 1 ? "open ⤢" : `${path.split("/").at(-1)} ⤢`}
        </button>
      ))}
    </span>
  );
}

export function CallBlock({
  call,
  onOpenFile,
}: {
  call: ToolCall;
  onOpenFile?: (path: string, callId: CallId) => void;
}) {
  const view = useMemo(() => callView(call.name, call.input), [call]);
  const narrow = useNarrow();
  const paths = useMemo(
    () =>
      onOpenFile === undefined ? [] : openablePaths(call.name, call.input),
    [call, onOpenFile],
  );
  // The call's body (a write's file contents, an edit's anchor and
  // replacement) truncates on narrow like a result does — the args tab of
  // the tap-to-inspect sheet holds the whole thing.
  const body = useMemo(() => {
    if (view.body === null) return null;
    if (!narrow) return { text: view.body.text, hidden: 0 };
    const lines = view.body.text.split("\n");
    if (lines.length <= NARROW_TRUNCATE_AT + 2)
      return { text: view.body.text, hidden: 0 };
    return {
      text: lines.slice(0, NARROW_TRUNCATE_AT).join("\n"),
      hidden: lines.length - NARROW_TRUNCATE_AT,
    };
  }, [view, narrow]);
  return (
    <div className="block">
      <CallHead
        name={call.name}
        summary={view.summary}
        trailing={
          onOpenFile !== undefined && (
            <OpenFileLinks
              paths={paths}
              callId={call.id}
              onOpenFile={onOpenFile}
            />
          )
        }
      />
      {view.intent !== null && <div className="intent">{view.intent}</div>}
      {body !== null && (
        <Code text={body.text} lang={view.body?.lang ?? null} />
      )}
      {body !== null && body.hidden > 0 && <MoreLines hidden={body.hidden} />}
    </div>
  );
}

/** The truncation marker. On narrow it names the affordance — there is no
 * hover on touch, so nothing else says the body is one tap away. */
function MoreLines({ hidden }: { hidden: number }) {
  const narrow = useNarrow();
  return (
    <div className="more-lines dim">
      ⋯ {hidden} more lines{narrow && " · tap to inspect"}
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

/** The phone's tighter budget, and it applies to every tool: at ~400pt a
 * long result is minutes of scrolling, so the transcript shows a glance —
 * header, a few lines, the kept count, the note — and the tap-to-inspect
 * sheet holds the body (the drill-down posture, docs/web-client.md). */
const NARROW_TRUNCATE_AT = 4;

/** Truncate a result's body for the transcript view — read/write at desktop
 * width, every tool at phone width. Render-whole survives this: a gate
 * decides a call, never a result, so no pending decision rests on a
 * truncated body — and the full body is one click away
 * (docs/web-client.md). */
function truncateBody(
  body: ResultView,
  tool: string | null,
  narrow: boolean,
): { body: ResultView; hidden: number } {
  if (!narrow && tool !== "read" && tool !== "write")
    return { body, hidden: 0 };
  const limit = narrow ? NARROW_TRUNCATE_AT : TRUNCATE_AT;
  if (
    (body.kind === "numbered" || body.kind === "diff") &&
    body.lines.length > limit + 2
  ) {
    return {
      body: { ...body, lines: body.lines.slice(0, limit) } as ResultView,
      hidden: body.lines.length - limit,
    };
  }
  if (body.kind === "code" || body.kind === "text") {
    const lines = body.text.split("\n");
    if (lines.length > limit + 2) {
      return {
        body: { ...body, text: lines.slice(0, limit).join("\n") },
        hidden: lines.length - limit,
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
  seq,
  raw,
  wire,
}: {
  tool: string | null;
  /** The result entry's seq — the address images resolve through. */
  seq: number;
  raw: RawResult;
  wire: WireState;
}) {
  // An oversized result arrives truncated (docs/control-api.md); the whole one
  // is one immutable fetch away, on demand rather than eagerly — most truncated
  // results are scrolled past, and fetching them all would restore the head-of-
  // line problem the elision removed.
  const whole = useWholeResult(seq, raw);
  const shown = whole.raw ?? raw;
  // The cordon parses the whole raw result — sentinel stripping, diff
  // classification, the kept-line filter. A store update re-renders every
  // entry, so without this a streamed token re-parses every result in the
  // session.
  const view = useMemo(
    () => resultRender(tool, shown, wire),
    [tool, shown, wire],
  );
  const narrow = useNarrow();
  const { body, hidden } = useMemo(
    () => truncateBody(view.body, tool, narrow),
    [view, tool, narrow],
  );
  // The head names the state and the size — except when triage kept every
  // line, where both go quiet (`stateWord` null) and the head is bare: the
  // result is the tool's result, whole, and nothing implies otherwise.
  const parts = [
    view.stateWord,
    view.sizeLabel,
    view.elisionLabel,
    view.isError ? "error" : null,
  ].filter((part) => part !== null);
  const label = parts.join(", ");
  return (
    <div className="block">
      {/* No inspect link: the whole call is the inspection affordance, and
          the call's hover state is the discovery surface (Transcript.tsx). */}
      <div className="kind result-head">
        {parts.length > 0 ? `result (${label})` : "result"}
      </div>
      <ResultBody body={body} />
      {(shown.images ?? []).map((image, idx) => (
        // The view carries only the reference (docs/control-api.md); the
        // bytes come from the image route, cookie-authed and
        // immutably-cached by the browser. width/height reserve layout
        // space before the fetch lands.
        <img
          key={idx}
          className="result-image"
          src={`/v1/log/${seq}/images/${idx}`}
          width={image.width}
          height={image.height}
          alt={`${image.media_type} ${image.width}×${image.height}`}
          loading="lazy"
        />
      ))}
      {hidden > 0 && <MoreLines hidden={hidden} />}
      {whole.elided !== undefined && (
        // The affordance is the honest half of the marker in the head: the
        // reader is looking at a prefix, and this is where the rest lives.
        <div className="note">
          <button
            type="button"
            className="linkish"
            disabled={whole.state === "fetching"}
            onClick={whole.fetchWhole}
          >
            {whole.state === "fetching"
              ? "fetching the whole result…"
              : whole.state === "failed"
                ? "the whole result did not arrive — retry"
                : `show the whole result (${byteSize(whole.elided.total_bytes)})`}
          </button>
        </div>
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
