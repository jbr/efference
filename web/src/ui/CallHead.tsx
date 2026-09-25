// A call's header row: the signature as `signatureLayout.ts` laid it out, with
// any argument the chip table upgrades rendered as its chip instead of its
// text (`chips.tsx`).
//
// A flat signature is one inline flow, so an estimate that ran long wraps as
// text rather than overflowing. A broken one is a block per row, indented in
// `ch` — the same unit the layout counted in — and each row still wraps as
// text if a wide character made it run over.
//
// Kept apart from ToolBlocks.tsx so it imports nothing but React, the layout,
// and the chip renderer it is handed: the markup is what the wrapping test
// asserts on, and it renders without a highlighter or a media query.

import type { SigArg } from "./signature.ts";
import type { SigLayout, SigToken } from "./signatureLayout.ts";

/** How a signature renders one argument's value. Absent — the streaming and
 * inspector heads — is the plain text; the transcript hands in `ArgValue`
 * (`chips.tsx`), which is that text upgraded to a chip for the few arguments
 * the table names. */
export type ChipRenderer = (arg: SigArg, text: string) => React.ReactNode;

const CLASS: Record<SigToken["kind"], string> = {
  server: "call-server",
  tool: "call-tool",
  sep: "call-sep",
  key: "call-arg-key",
  value: "call-arg-value",
  target: "call-target",
};

function Token({ token, chip }: { token: SigToken; chip?: ChipRenderer }) {
  return (
    <span className={CLASS[token.kind]}>
      {token.arg !== undefined && chip !== undefined
        ? chip(token.arg, token.text)
        : token.text}
    </span>
  );
}

/** The signature's tokens: `ferritin.search(crate: "serde", query: "…")`, a
 * bare `bash · ls -la`, or — broken — a row per argument. The server segment
 * is dimmed rather than sized: built-in versus mounted-server is the
 * `open_world_hint` distinction, i.e. whether the call could have reached
 * outside the session. */
export function CallSignature({
  layout,
  chip,
}: {
  layout: SigLayout;
  chip?: ChipRenderer;
}) {
  if (!layout.broken) {
    return layout.rows[0].tokens.map((token, i) => (
      <Token key={i} token={token} chip={chip} />
    ));
  }
  return layout.rows.map((row, i) => (
    <span
      key={i}
      className="call-row"
      style={{ paddingLeft: `${row.indent}ch` }}
    >
      {row.tokens.map((token, j) => (
        <Token key={j} token={token} chip={chip} />
      ))}
    </span>
  ));
}

/** The transcript's header row. One shape for committed and streaming calls,
 * so a call does not change appearance at commit. `measure` is the ref the
 * column count is read from (`columns.ts`). */
export function CallHead({
  layout,
  arriving,
  chip,
  measure,
}: {
  layout: SigLayout;
  arriving?: boolean;
  chip?: ChipRenderer;
  measure?: (element: HTMLElement | null) => void;
}) {
  return (
    <div className="call-head" ref={measure}>
      <span className={layout.broken ? "call-sig broken" : "call-sig"}>
        <span className="prompt">›</span>{" "}
        <CallSignature layout={layout} chip={chip} />
        {arriving === true && <span className="dim"> …</span>}
      </span>
    </div>
  );
}
