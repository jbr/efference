// Tool call and result blocks (docs/web-client.md): the display half over
// the cordon's parse (`render/parse.ts`). A call renders as its intent, then
// a pseudocode signature laid out at the column's width (`signature.ts`,
// `signatureLayout.ts`, `CallHead.tsx`), then any argument that is content
// rather than an argument as a block under its key; a result renders its tool-specific
// view — colored diff lines, framing-stripped file content, numbered kept
// lines — with the raw content one tap away whenever the body isn't
// verbatim raw. `data-lang` on code bodies is the future highlighter's hook.

import { useMemo } from "react";
import { join } from "../api/base.ts";
import type {
  CallId,
  DistillateLines,
  RawResult,
  ToolCall,
  WireState,
} from "../api/types.ts";
import { Code, NumberedCode, StreamingCode } from "./Code.tsx";
import { PinnedBox } from "./TranscriptItems.tsx";
import {
  hasStructure,
  partialInput,
  streamingLang,
} from "../render/partial.ts";
import { byteSize, resultRender, type ResultView } from "../render/parse.ts";
import { ArgValue } from "./chips.tsx";
import { chipExtra } from "./chipTable.ts";
import { CallHead } from "./CallHead.tsx";
import { useColumns } from "./columns.ts";
import { bodyOf, callView, type CallBody, type CallView } from "./signature.ts";
import { layoutSignature, type SigLayout } from "./signatureLayout.ts";
import { InlineMarkdown, Markdown } from "./Markdown.tsx";
import { truncateBody, truncateText } from "./truncate.ts";
import { useNarrow } from "./useNarrow.ts";
import { useWholeResult } from "./wholeResult.ts";

/** A call's signature laid out at the width of the head it renders in, and
 * the bodies below it: the view's content arguments, then any argument too
 * long to sit in the signature at this width. `tool` is the wire name the
 * chip table is keyed by; null lays out with no chip allowance. */
function useSignature(
  view: CallView,
  tool: string | null,
): {
  measure: (element: HTMLElement | null) => void;
  layout: SigLayout;
  bodies: CallBody[];
} {
  const [measure, columns] = useColumns();
  const layout = useMemo(
    () =>
      layoutSignature(view, columns, (arg) =>
        tool === null ? 0 : chipExtra(tool, arg.key, arg.value),
      ),
    [view, columns, tool],
  );
  const bodies = useMemo(
    () => [
      ...view.bodies,
      ...layout.pulled.map((arg) => bodyOf(arg.key, arg.value, null)),
    ],
    [view, layout],
  );
  return { measure, layout, bodies };
}

/** The arguments that are content, each under its key: a file's contents
 * highlighted in its language, anything else verbatim with its own line
 * breaks. */
function CallBodies({
  bodies,
  truncate,
}: {
  bodies: CallBody[];
  /** The narrow flag when bodies truncate (a committed call); null renders
   * them whole (one still streaming, which is short by construction). */
  truncate: boolean | null;
}) {
  if (bodies.length === 0) return null;
  return (
    <dl className="call-bodies">
      {bodies.map((body, i) => {
        const { text, hidden } =
          truncate === null
            ? { text: body.text, hidden: 0 }
            : truncateText(body.text, truncate);
        return (
          <div key={body.key ?? i} className="call-body">
            {body.key !== null && <dt>{body.key}</dt>}
            {/* bash's own multi-line command: it carries the prompt the
                head would have. */}
            <dd className={body.key === null ? "shell-command" : undefined}>
              {body.prose === true ? (
                <Markdown>{text}</Markdown>
              ) : body.lang === null ? (
                <div className="call-body-text">{text}</div>
              ) : (
                <Code text={text} lang={body.lang} />
              )}
              {hidden > 0 && <MoreLines hidden={hidden} />}
            </dd>
          </div>
        );
      })}
    </dl>
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
  const { measure, layout, bodies } = useSignature(view, call.name);
  // A call's content arguments (a write's contents, a long message) truncate
  // on the same budget a result does, at both widths — the inspector's args
  // tab holds the whole thing.
  const narrow = useNarrow();
  return (
    <div className="block">
      {/* The intent is the call's title (docs/web-client.md "Detail by
          recency"): what it was for, then what it was. */}
      {view.intent !== null && (
        <div className="intent">
          <InlineMarkdown>{view.intent}</InlineMarkdown>
        </div>
      )}
      {/* The chip table decides which arguments render as more than their
          text (chipTable.ts): the file link rides the path argument it opens,
          and a `p#<n>` resolves to the command it stood for. */}
      <CallHead
        layout={layout}
        measure={measure}
        chip={(arg, text) => (
          <ArgValue call={call} arg={arg} text={text} onOpenFile={onOpenFile} />
        )}
      />
      <CallBodies bodies={bodies} truncate={narrow} />
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
 * so the call fills in as bytes land rather than changing shape at commit.
 * The in-flight value never reaches it: what [`callView`] produces is an
 * identity claim (`write · src/api.rs`), and only a value whose closing quote
 * has arrived may back one — except the intent, which claims nothing about
 * what the call touches and fills in as the title while it streams. Before
 * anything structural arrives, the call is its bare name.
 */
export function LiveCallBlock({
  name,
  input,
}: {
  name: string;
  input: string;
}) {
  const parsed = useMemo(() => partialInput(input), [input]);
  const structured = parsed !== null && hasStructure(parsed);
  // `args: null` renders the bare name while nothing structural has arrived:
  // `write()` would claim there are no arguments.
  const view = useMemo(
    () =>
      structured
        ? callView(name, parsed.settled)
        : { ...callView(name, undefined), args: null },
    [name, parsed, structured],
  );
  const { measure, layout, bodies } = useSignature(view, null);
  if (!structured) {
    return (
      <div className="block">
        <CallHead layout={layout} measure={measure} arriving />
      </div>
    );
  }
  const { arriving } = parsed;
  // The same layout the committed call has (CallBlock), so a call does not
  // change shape when it commits: the intent is the title — filling in as it
  // streams, when it is the value in flight — then the signature, then the
  // bodies.
  const intent =
    view.intent ?? (arriving?.key === "intent" ? arriving.text : null);
  return (
    <div className="block">
      {intent !== null && (
        <div className="intent">
          <InlineMarkdown>{intent}</InlineMarkdown>
        </div>
      )}
      <CallHead layout={layout} measure={measure} arriving />
      <CallBodies bodies={bodies} truncate={null} />
      {arriving !== null &&
        arriving.key !== "intent" &&
        (arriving.text.includes("\n") ? (
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
  distillate,
}: {
  tool: string | null;
  /** The result entry's seq — the address images resolve through. */
  seq: number;
  raw: RawResult;
  wire: WireState;
  distillate?: DistillateLines;
}) {
  // An oversized result arrives truncated (docs/control-api.md); the whole one
  // is one fetch away, on demand rather than eagerly — most truncated
  // results are scrolled past, and fetching them all would restore the head-of-
  // line problem the elision removed.
  const whole = useWholeResult(seq, raw);
  const shown = whole.raw ?? raw;
  // The cordon parses the whole raw result — sentinel stripping, diff
  // classification, the kept-line filter. A store update re-renders every
  // entry, so without this a streamed token re-parses every result in the
  // session.
  const view = useMemo(
    () => resultRender(tool, shown, wire, distillate),
    [tool, shown, wire, distillate],
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
      {wire === "Pending" ? (
        // Raw output while triage runs, in the bounded box a running
        // command's tail uses; the verdict replaces it with the triaged body.
        <PinnedBox grows={shown.content} className="t-output">
          <ResultBody body={body} />
        </PinnedBox>
      ) : (
        <ResultBody body={body} />
      )}
      {(shown.images ?? []).map((image, idx) => (
        // The view carries only the reference (docs/control-api.md); the
        // bytes come from the image route, cookie-authed and
        // ETag-validated by the browser. width/height reserve layout
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
          <span className="note-text">
            <InlineMarkdown>{view.note}</InlineMarkdown>
          </span>
        </div>
      )}
      {/* The per-result raw <details> is gone: raw is the inspector's tab
          now, one click on the call away. */}
    </div>
  );
}
