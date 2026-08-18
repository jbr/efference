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

import { Fragment, useMemo } from "react";
import type { Token } from "./highlight.ts";
import { useHighlight, useStreamingTokens } from "./highlight.ts";

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
  // Memoized on the text, not recomputed per render: a store update
  // re-renders the whole transcript, and tokenizing every committed block on
  // every streamed token is what made the page stop responding mid-stream.
  const lines = useMemo(
    () => (highlight && lang !== null ? highlight(text, lang) : null),
    [highlight, text, lang],
  );
  return <CodeBody text={text} lang={lang} lines={lines} />;
}

/** [`Code`] for a value still arriving: the text grows at the end on every
 * delta, so the memo above would re-tokenize the whole buffer per chunk —
 * O(n²) over a large file write. The streaming tokenizer keeps the finished
 * lines' tokens and does only the arriving tail (`highlight.ts`). Same
 * markup, so the block does not change appearance when the call commits and
 * this component hands over to [`Code`]. */
export function StreamingCode({
  text,
  lang,
}: {
  text: string;
  lang: string | null;
}) {
  const lines = useStreamingTokens(text, lang);
  return <CodeBody text={text} lang={lang} lines={lines} />;
}

function CodeBody({
  text,
  lang,
  lines,
}: {
  text: string;
  lang: string | null;
  lines: Token[][] | null;
}) {
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
  // `lines` is rebuilt by the cordon on every render, so it cannot be a memo
  // dependency — joining is cheap, and the string it produces compares by
  // value, which is what keeps the tokenizing off the render path.
  const text = lines.map(([, line]) => line).join("\n");
  const tokens = useMemo(
    () => (highlight && lang !== null ? highlight(text, lang) : null),
    [highlight, text, lang],
  );
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
