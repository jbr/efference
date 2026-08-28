// How much of a body the transcript shows. One budget for a call's elided
// argument and for a result, because they are the same reading problem: the
// inspector holds the whole thing either way (docs/web-client.md).

import type { ResultView } from "../render/parse.ts";

/** Desktop budget. Slack of 2 so the `⋯ N more lines` indicator never stands
 * in for fewer lines than it occupies. */
export const TRUNCATE_AT = 12;

/** The phone's tighter budget, and it applies to every tool: at ~400pt a long
 * body is minutes of scrolling, so the transcript shows a glance and the
 * tap-to-inspect sheet holds the rest. */
export const NARROW_TRUNCATE_AT = 4;

/** Truncate a block of text to the width's budget. */
export function truncateText(
  text: string,
  narrow: boolean,
): { text: string; hidden: number } {
  const limit = narrow ? NARROW_TRUNCATE_AT : TRUNCATE_AT;
  const lines = text.split("\n");
  if (lines.length <= limit + 2) return { text, hidden: 0 };
  return {
    text: lines.slice(0, limit).join("\n"),
    hidden: lines.length - limit,
  };
}

/** Truncate a result's body — read/write at desktop width, every tool at phone
 * width. Render-whole survives this: a gate decides a call, never a result, so
 * no pending decision rests on a truncated body — and the full body is one
 * click away. */
export function truncateBody(
  body: ResultView,
  tool: string | null,
  narrow: boolean,
): { body: ResultView; hidden: number } {
  if (!narrow && tool !== "read" && tool !== "write")
    return { body, hidden: 0 };
  const limit = narrow ? NARROW_TRUNCATE_AT : TRUNCATE_AT;
  if (
    (body.kind === "numbered" || body.kind === "diff") &&
    body.lines.length > limit + 2
  ) {
    return {
      body: { ...body, lines: body.lines.slice(0, limit) } as ResultView,
      hidden: body.lines.length - limit,
    };
  }
  if (body.kind === "code" || body.kind === "text") {
    const { text, hidden } = truncateText(body.text, narrow);
    if (hidden > 0) return { body: { ...body, text }, hidden };
  }
  return { body, hidden: 0 };
}
