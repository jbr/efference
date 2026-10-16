import { Fragment, useMemo, useState } from "react";
import { useHighlight } from "./highlight.ts";
import { rustLines } from "../render/triage.ts";
import "./RangeContent.css";
import { mergeRanges, type LineRange } from "./lineRanges.ts";

type Props = {
  text: string;
  retained: readonly LineRange[];
  lang?: string | null;
  step?: number;
  startLine?: number;
  /** Display coordinates when framing rows have been removed from a result. */
  numbers?: readonly number[];
  /** Visibility can be budgeted independently of the curation selection. */
  initial?: readonly LineRange[];
  lineClass?: (n: number) => string | undefined;
  variant?: "transcript";
};

/** Original captured content with locally expandable retained ranges. Expansion
 * is display-only; it does not change curation (docs/web-client.md). */
export function RangeContent(props: Props) {
  const identity = JSON.stringify([
    props.text,
    props.retained,
    props.startLine,
    props.initial,
    props.numbers,
  ]);
  return <RangeView key={identity} {...props} />;
}

function RangeView({
  text,
  retained,
  lang = null,
  step = 10,
  startLine = 1,
  numbers,
  initial = retained,
  lineClass,
  variant,
}: Props) {
  const lines = useMemo(() => rustLines(text), [text]);
  const kept = useMemo(
    () => mergeRanges(retained, lines.length),
    [retained, lines.length],
  );
  const [visible, setVisible] = useState(() =>
    mergeRanges(initial, lines.length),
  );
  const highlight = useHighlight();
  const tokens = useMemo(
    () => (highlight && lang ? highlight(text, lang) : null),
    [highlight, text, lang],
  );
  const chunk = Math.max(1, Math.floor(step));
  const number = (n: number) => numbers?.[n - 1] ?? startLine + n - 1;
  const gap = (a: number, b: number, above: boolean, below: boolean) => {
    if (a > b) return null;
    const both = above && below;
    const count = Math.min(b - a + 1, chunk * (both ? 2 : 1));
    return (
      <button
        type="button"
        className="range-gap"
        key={`gap-${a}-${b}`}
        aria-label={`Show ${count} omitted lines${both ? " from both ends" : below ? " above" : " below"}`}
        onClick={() =>
          setVisible((prev) =>
            mergeRanges(
              [
                ...prev,
                ...(above || !below
                  ? [[a, Math.min(b, a + chunk - 1)] as const]
                  : []),
                ...(below ? [[Math.max(a, b - chunk + 1), b] as const] : []),
              ],
              lines.length,
            ),
          )
        }
      >
        <span className="range-gap-icon" aria-hidden="true">
          {both ? "⇅" : below ? "↑" : "↓"}
        </span>
        <span>{b - a + 1} lines omitted</span>
      </button>
    );
  };
  return (
    <div
      className={`range-content${variant ? ` range-${variant}` : ""}`}
      data-lang={lang ?? undefined}
    >
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
                  const isKept = kept.some(([s, e]) => n >= s && n <= e);
                  return (
                    <div
                      className={`numbered-line range-line${isKept ? " is-retained" : ""}${lineClass?.(number(n)) ? ` ${lineClass(number(n))}` : ""}`}
                      data-line={number(n)}
                      key={n}
                    >
                      <span
                        className="lineno"
                        title={
                          isKept ? "Retained by curation" : "Revealed context"
                        }
                      >
                        {number(n)}
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
