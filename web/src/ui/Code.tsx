// Code bodies, highlighted when the language is known (docs/web-client.md).
//
// One rule carried over from the TUI: highlight **only when the language is
// known**, and leave an untagged block as plain text. The TUI's renderer
// used to default to `rust`, which was right in ferritin (a rustdoc fence is
// Rust unless it says otherwise) and wrong here, where the commonest
// untagged block is a tool result — `ls` output was labelled and coloured as
// Rust (docs/tui.md).
//
// `data-lang` stays on the element whether or not highlighting ran: it is
// the record of what the cordon inferred, independent of what got coloured.

import { Fragment } from "react";
import type { Token } from "./highlight.ts";
import { useHighlight } from "./highlight.ts";

function Line({ tokens }: { tokens: Token[] }) {
  return (
    <>
      {tokens.map((token, i) => (
        <span key={i} style={token.color ? { color: token.color } : undefined}>
          {token.content}
        </span>
      ))}
    </>
  );
}

/** A plain code body.
 *
 * Lines are joined by explicit newline text rather than wrapped in one
 * element each: inside a `<pre>`, an element per line gives a blank line
 * zero height, so every empty line in the source silently disappears. The
 * newlines keep this byte-identical to the unhighlighted `<pre>` it
 * replaces. */
export function Code({ text, lang }: { text: string; lang: string | null }) {
  const highlight = useHighlight();
  const lines = highlight && lang !== null ? highlight(text, lang) : null;
  return (
    <pre data-lang={lang ?? undefined}>
      {lines === null
        ? text
        : lines.map((tokens, i) => (
            <Fragment key={i}>
              {i > 0 && "\n"}
              <Line tokens={tokens} />
            </Fragment>
          ))}
    </pre>
  );
}

/** A code body with the result's own line numbers in the gutter, the jumps
 * between them stating what triage dropped.
 *
 * The kept lines are highlighted as one document, so a grammar that spans a
 * dropped region can mis-lex after the gap. Tokenizing each line
 * independently would be worse (every multi-line construct would break), and
 * the alternative — highlighting the whole raw result and then selecting —
 * needs the unfiltered text, which this view does not carry. */
export function NumberedCode({
  lines,
  lang,
}: {
  lines: [number, string][];
  lang: string | null;
}) {
  const highlight = useHighlight();
  const text = lines.map(([, line]) => line).join("\n");
  const tokens = highlight && lang !== null ? highlight(text, lang) : null;
  return (
    <pre data-lang={lang ?? undefined}>
      {lines.map(([n, line], i) => (
        <div key={n}>
          <span className="lineno">{String(n).padStart(5)} </span>
          {tokens?.[i] === undefined ? line : <Line tokens={tokens[i]} />}
        </div>
      ))}
    </pre>
  );
}
