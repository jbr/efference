import { Fragment, useMemo, useState } from "react";
import { useHighlight } from "./highlight.ts";
import "./RangeContent.css";

import { mergeRanges, type LineRange } from "./lineRanges.ts";

/** Original captured content with locally expandable retained ranges. Expansion
 * is display-only; it does not change curation (docs/web-client.md). */
export function RangeContent({
  text,
  retained,
  lang = null,
  step = 12,
  startLine = 1,
}: {
  text: string;
  retained: readonly LineRange[];
  lang?: string | null;
  step?: number;
  startLine?: number;
}) {
  // A different captured source/range selection starts a fresh expansion view.
  const identity = JSON.stringify([text, retained, startLine]);
  return (
    <RangeView
      key={identity}
      text={text}
      retained={retained}
      lang={lang}
      step={Math.max(1, Math.floor(step))}
      startLine={startLine}
    />
  );
}
function RangeView({
  text,
  retained,
  lang,
  step,
  startLine,
}: {
  text: string;
  retained: readonly LineRange[];
  lang: string | null;
  step: number;
  startLine: number;
}) {
  const lines = useMemo(() => (text === "" ? [] : text.split("\n")), [text]);
  const kept = useMemo(
    () => mergeRanges(retained, lines.length),
    [retained, lines.length],
  );
  const [visible, setVisible] = useState<LineRange[]>(kept);
  const highlight = useHighlight();
  const tokens = useMemo(
    () => (highlight && lang ? highlight(text, lang) : null),
    [highlight, text, lang],
  );
  const reveal = (range: LineRange) =>
    setVisible((prev) => mergeRanges([...prev, range], lines.length));
  const gap = (a: number, b: number, above: boolean, below: boolean) =>
    a > b ? null : (
      <div className="range-gap" key={`gap-${a}-${b}`}>
        <div className="range-gap-actions">
          {above && (
            <button
              aria-label={`Show ${Math.min(step, b - a + 1)} lines after line ${startLine + a - 2}`}
              title="Expand downward"
              onClick={() => reveal([a, Math.min(b, a + step - 1)])}
            >
              ↓
            </button>
          )}
          {below && (
            <button
              aria-label={`Show ${Math.min(step, b - a + 1)} lines before line ${startLine + b}`}
              title="Expand upward"
              onClick={() => reveal([Math.max(a, b - step + 1), b])}
            >
              ↑
            </button>
          )}
          {!above && !below && (
            <button
              aria-label="Show first lines"
              onClick={() => reveal([a, Math.min(b, a + step - 1)])}
            >
              ↓
            </button>
          )}
        </div>
        <span>{b - a + 1} lines omitted</span>
      </div>
    );
  return (
    <div className="range-content" data-lang={lang ?? undefined}>
      {visible.length === 0
        ? gap(1, lines.length, false, false)
        : visible.map(([a, b], index) => (
            <Fragment key={a}>
              {gap(
                index === 0 ? 1 : visible[index - 1][1] + 1,
                a - 1,
                index > 0,
                true,
              )}
              <pre>
                {lines.slice(a - 1, b).map((line, i) => {
                  const n = a + i;
                  const isKept = kept.some(
                    ([start, end]) => n >= start && n <= end,
                  );
                  return (
                    <div
                      className={`numbered-line range-line${isKept ? " is-retained" : ""}`}
                      data-line={startLine + n - 1}
                      key={n}
                    >
                      <span
                        className="lineno"
                        title={
                          isKept ? "Retained by curation" : "Revealed context"
                        }
                      >
                        {startLine + n - 1}
                      </span>
                      <span className="line-body">
                        {tokens?.[n - 1]
                          ? tokens[n - 1].map((token, j) => (
                              <span
                                key={j}
                                style={
                                  token.color
                                    ? { color: token.color }
                                    : undefined
                                }
                              >
                                {token.content}
                              </span>
                            ))
                          : line || "\u00a0"}
                      </span>
                    </div>
                  );
                })}
              </pre>
            </Fragment>
          ))}
      {visible.length > 0 &&
        gap(visible.at(-1)![1] + 1, lines.length, true, false)}
    </div>
  );
}
