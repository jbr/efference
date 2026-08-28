// A call's header row: the pseudocode signature the cordon computed
// (`render/parse.ts`), plus whatever affordance rides at the end of the row.
//
// The markup carries the wrapping rule. `.call-head` is a flex row so the
// trailing slot stays placed at the end, and flex items are blockified by the
// CSS Display spec — so a signature made of sibling spans wrapped as whole
// boxes onto their own lines instead of flowing as text. Everything the reader
// reads as one sentence therefore sits inside **one** flex item
// (`.call-sig`), whose children are ordinary inline spans and wrap as text at
// every width (docs/web-client.md).
//
// Kept apart from ToolBlocks.tsx so it imports nothing but React and the
// cordon: the markup is what the wrapping test asserts on, and it renders
// without a highlighter or a media query.

import type { CallView } from "../render/parse.ts";

/** The signature itself: `ferritin.search(crate: "serde", query: "…")`, or a
 * bare `bash · ls -la`. The server segment is dimmed rather than sized —
 * built-in versus mounted-server is the `open_world_hint` distinction, i.e.
 * whether the call could have reached outside the session. */
export function CallSignature({ view }: { view: CallView }) {
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
                {arg.text}
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
  trailing,
}: {
  view: CallView;
  arriving?: boolean;
  trailing?: React.ReactNode;
}) {
  return (
    <div className="call-head">
      <span className="call-sig">
        <span className="prompt">›</span> <CallSignature view={view} />
        {arriving === true && <span className="dim"> …</span>}
      </span>
      {trailing}
    </div>
  );
}
