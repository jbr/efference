// Tool call and result blocks (docs/web-client.md): the display half over
// the cordon's parse (`render/parse.ts`). A call renders as a pseudocode
// signature (`CallHead.tsx`) with its intent below it and any argument too
// big for the signature as a block; a result renders its tool-specific
// view — colored diff lines, framing-stripped file content, numbered kept
// lines — with the raw content one tap away whenever the body isn't
// verbatim raw. `data-lang` on code bodies is the future highlighter's hook.

import { useMemo } from "react";
import { join } from "../api/base.ts";
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
import { ArgValue } from "./chips.tsx";
import { CallHead } from "./CallHead.tsx";
import { truncateBody, truncateText } from "./truncate.ts";
import { useNarrow } from "./useNarrow.ts";
import { useWholeResult } from "./wholeResult.ts";

export function CallBlock({
  call,
  onOpenFile,
}: {
  call: ToolCall;
  onOpenFile?: (path: string, callId: CallId) => void;
}) {
  const view = useMemo(() => callView(call.name, call.input), [call]);
  const narrow = useNarrow();
  // A call's elided argument (a write's contents, a nested envelope) truncates
  // on the same budget a result does, at both widths — the signature already
  // states its shape, and the inspector's args tab holds the whole thing.
  const bodies = useMemo(
    () =>
      view.bodies.map((body) => ({
        ...body,
        ...truncateText(body.text, narrow),
      })),
    [view, narrow],
  );
  return (
    <div className="block">
      {/* The chip table decides which arguments render as more than their
          text (chipTable.ts): the file link rides the path argument it opens,
          and a `p#<n>` resolves to the command it stood for. */}
      <CallHead
        view={view}
        chip={(arg) => (
          <ArgValue call={call} arg={arg} onOpenFile={onOpenFile} />
        )}
      />
      {view.intent !== null && <div className="intent">{view.intent}</div>}
      {bodies.map((body, i) => (
        <div key={body.key ?? i} className="call-body">
          {body.key !== null && <div className="call-body-key">{body.key}</div>}
          <Code text={body.text} lang={body.lang} />
          {body.hidden > 0 && <MoreLines hidden={body.hidden} />}
        </div>
      ))}
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
        {/* `args: null` renders the bare name: nothing structural has
            arrived, and `write()` would claim there are no arguments. */}
        <CallHead
          view={{ ...callView(name, undefined), args: null }}
          arriving
        />
        <pre className="dim">{input}</pre>
      </div>
    );
  }
  const view = callView(name, parsed.settled);
  const { arriving } = parsed;
  return (
    <div className="block">
      <CallHead view={view} arriving />
      {view.intent !== null && <div className="intent">{view.intent}</div>}
      {view.bodies.map((body, i) => (
        <div key={body.key ?? i} className="call-body">
          {body.key !== null && <div className="call-body-key">{body.key}</div>}
          <Code text={body.text} lang={body.lang} />
        </div>
      ))}
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
          src={join(`/v1/log/${seq}/images/${idx}`)}
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
