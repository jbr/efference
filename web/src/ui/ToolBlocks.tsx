// Tool call and result blocks (docs/web-client.md): the display half over
// the cordon's parse (`render/parse.ts`). A call leads with its intent and
// summarizes its identifying argument; a result renders its tool-specific
// view — colored diff lines, framing-stripped file content, numbered kept
// lines — with the raw content one tap away whenever the body isn't
// verbatim raw. `data-lang` on code bodies is the future highlighter's hook.

import type { RawResult, ToolCall, WireState } from "../api/types.ts";
import {
  callView,
  resultRender,
  wireLabel,
  type ResultView,
} from "../render/parse.ts";

export function CallBlock({ call }: { call: ToolCall }) {
  const view = callView(call.name, call.input);
  return (
    <div className="block">
      <div className="kind">
        tool: {call.name}
        {view.summary !== null && <> · {view.summary}</>}
      </div>
      {view.intent !== null && <div className="dim intent">{view.intent}</div>}
      {view.body !== null && (
        <pre data-lang={view.body.lang ?? undefined}>{view.body.text}</pre>
      )}
    </div>
  );
}

function Body({ body }: { body: ResultView }) {
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
      return <pre data-lang={body.lang ?? undefined}>{body.text}</pre>;
    case "numbered":
      return (
        <pre data-lang={body.lang ?? undefined}>
          {body.lines.map(([n, text]) => (
            <div key={n}>
              <span className="lineno">{String(n).padStart(5)} </span>
              {text}
            </div>
          ))}
        </pre>
      );
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
  const view = resultRender(tool, raw, wire);
  const label = [wireLabel(wire), view.sizeLabel, view.isError ? "error" : null]
    .filter((part) => part !== null)
    .join(", ");
  return (
    <div className="block">
      <div className="kind">result ({label})</div>
      <Body body={view.body} />
      {view.note !== null && <div className="dim">⟢ {view.note}</div>}
      {view.rawDiffers && (
        <details className="dim">
          <summary className="kind">raw</summary>
          <pre>{raw.content}</pre>
        </details>
      )}
    </div>
  );
}
