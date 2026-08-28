// A call's header row: the pseudocode signature the cordon computed
// (`render/parse.ts`), with any argument the chip table upgrades rendered as
// its chip instead of its text (`chips.tsx`).
//
// The row is a plain block. Everything the reader reads as one sentence is one
// inline flow — the signature and its chips alike — so it wraps as text at
// every width; the head was a flex row only while an affordance sat at the end
// of it, and a chip attaches to the argument it refers to instead
// (docs/web-client.md).
//
// Kept apart from ToolBlocks.tsx so it imports nothing but React, the cordon,
// and the chip renderer it is handed: the markup is what the wrapping test
// asserts on, and it renders without a highlighter or a media query.

import type { CallView, SigArg } from "../render/parse.ts";

/** How a signature renders one argument's value. Absent — the streaming and
 * inspector heads — is the plain text the cordon computed; the transcript
 * hands in `ArgValue` (`chips.tsx`), which is that text upgraded to a chip for
 * the few arguments the table names. */
export type ChipRenderer = (arg: SigArg) => React.ReactNode;

/** The signature itself: `ferritin.search(crate: "serde", query: "…")`, or a
 * bare `bash · ls -la`. The server segment is dimmed rather than sized —
 * built-in versus mounted-server is the `open_world_hint` distinction, i.e.
 * whether the call could have reached outside the session. */
export function CallSignature({
  view,
  chip,
}: {
  view: CallView;
  chip?: ChipRenderer;
}) {
  return (
    <>
      <span className="call-tool">
        {view.server !== null && (
          <span className="call-server">{view.server}.</span>
        )}
        {view.tool}
      </span>
      {view.args === null ? (
        view.target !== null && (
          <>
            {" "}
            <span className="call-sep">·</span>{" "}
            <span className="call-target">{view.target}</span>
          </>
        )
      ) : (
        <span className="call-args">
          {"("}
          {view.args.map((arg, i) => (
            <span key={arg.key} className="call-arg">
              {i > 0 && <span className="call-sep">, </span>}
              <span className="call-arg-key">{arg.key}</span>
              <span className="call-sep">: </span>
              <span
                className={arg.elided ? "call-arg-shape" : "call-arg-value"}
              >
                {chip === undefined ? arg.text : chip(arg)}
              </span>
            </span>
          ))}
          {")"}
        </span>
      )}
    </>
  );
}

/** The transcript's header row. One shape for committed and streaming calls,
 * so a call does not change appearance at commit. */
export function CallHead({
  view,
  arriving,
  chip,
}: {
  view: CallView;
  arriving?: boolean;
  chip?: ChipRenderer;
}) {
  return (
    <div className="call-head">
      <span className="call-sig">
        <span className="prompt">›</span>{" "}
        <CallSignature view={view} chip={chip} />
        {arriving === true && <span className="dim"> …</span>}
      </span>
    </div>
  );
}
