// ⚠ CORDON — a deliberate, temporary duplicate of the daemon's projection
// renderer (`src/project/render.rs`; docs/web-client.md "The cordon").
// The triaged view must show what the model saw, and until tool results
// are structured (audience-tagged content blocks, or per-tool result
// schemas), the only client-side source is a mirror of the Rust renderer.
// Everything under `web/src/render/` is planned deletion: when structured
// output lands, drop the directory whole. Ports are literal — same names,
// same order, same strings — so divergence is findable by diffing against
// render.rs; the tests mirror `src/project/test/triaged.rs` case for case.

import type { Interruption, Keep, RawResult, WireState } from "../api/types.ts";

/** Mirrors `TRIAGE_LINE_CAP` (src/project.rs). */
export const TRIAGE_LINE_CAP = 2000;

type Range = [number, number];

/** Mirrors Rust `str::lines`: split on \n, strip a trailing \r per line, no
 * phantom empty line after a trailing newline. Shared with parse.ts so both
 * halves of the cordon number lines identically. */
export function rustLines(content: string): string[] {
  if (content === "") return [];
  const parts = content.split("\n");
  if (parts.at(-1) === "") parts.pop();
  return parts.map((l) => (l.endsWith("\r") ? l.slice(0, -1) : l));
}

/** Mirrors `normalize`: clamp to [1, n], drop out-of-range, sort, merge
 * overlapping/adjacent, tolerate reversed. */
export function normalize(ranges: Range[], n: number): Range[] {
  if (n === 0) return [];
  const rs = ranges
    .map(([s, e]): Range => [Math.min(s, e), Math.max(s, e)])
    .filter(([s]) => s >= 1 && s <= n)
    .map(([s, e]): Range => [s, Math.min(e, n)])
    .sort((a, b) => a[0] - b[0]);
  const merged: Range[] = [];
  for (const [s, e] of rs) {
    const last = merged.at(-1);
    if (last && s <= last[1] + 1) last[1] = Math.max(last[1], e);
    else merged.push([s, e]);
  }
  return merged;
}

/** Mirrors `hunk_header`. */
const hunkHeader = (start: number, end: number) =>
  start === end ? `@@ line ${start} @@` : `@@ lines ${start}-${end} @@`;

/** Mirrors `NOTHING_KEPT`. */
export const NOTHING_KEPT = "(output dropped by triage)";

/** Mirrors `INTENT_FEEDBACK_LEAD`. */
export const INTENT_FEEDBACK_LEAD = "⟢ re your intent: ";

/** Mirrors `render_hunks`. */
export function renderHunks(
  lines: string[],
  ranges: Range[],
  note: string | null,
  intentFeedback?: string | null,
  total: number = lines.length,
): string {
  const norm = normalize(ranges, lines.length);
  const contentEnd = lines.findLastIndex((l) => l.trim() !== "") + 1;
  const first = norm[0];
  const spansEverything =
    norm.length === 1 && first[0] === 1 && first[1] >= contentEnd;
  const parts: string[] = [];
  if (norm.length === 0 && lines.length > 0) parts.push(NOTHING_KEPT);
  for (const [start, end] of norm) {
    if (!spansEverything) parts.push(hunkHeader(start, end));
    for (let i = start; i <= end; i++) parts.push(lines[i - 1]);
  }
  const last = norm.at(-1);
  if (!spansEverything && last && last[1] < contentEnd) {
    parts.push(`@@ ${total} lines total @@`);
  }
  const trimmed = note?.trim();
  if (trimmed) parts.push(`⟢ ${trimmed}`);
  const feedback = intentFeedback?.trim();
  if (feedback) parts.push(`${INTENT_FEEDBACK_LEAD}${feedback}`);
  if (parts.length === 0) parts.push("(nothing kept)");
  return parts.join("\n");
}

/** Mirrors `render_triaged`. */
export function renderTriaged(
  content: string,
  keep: Keep,
  note: string | null,
  intentFeedback?: string | null,
): string {
  const all = rustLines(content);
  const shown = all.slice(0, TRIAGE_LINE_CAP);
  const dropped = all.length - shown.length;
  const ranges: Range[] =
    keep === "All" ? [[1, shown.length]] : keep === "None" ? [] : keep.Ranges;
  let out = renderHunks(
    shown,
    ranges,
    note,
    intentFeedback,
    shown.length + dropped,
  );
  if (dropped > 0) {
    out += `\n… [${dropped} more lines truncated before triage]`;
  }
  return out;
}

/** A kept line paired with its 1-based line number in the raw content. */
export type NumberedLine = [number, string];

/** Mirrors `selection` + `Selection::numbered_lines`, plus the total the
 * TUI's "kept N of M lines" header divides by (shown + truncated). */
export function selectionKept(
  content: string,
  keep: Keep,
): { kept: NumberedLine[]; total: number } {
  const all = rustLines(content);
  const shown = all.slice(0, TRIAGE_LINE_CAP);
  const dropped = all.length - shown.length;
  const ranges = normalize(
    keep === "All" ? [[1, shown.length]] : keep === "None" ? [] : keep.Ranges,
    shown.length,
  );
  const kept: NumberedLine[] = [];
  for (const [s, e] of ranges) {
    for (let i = s; i <= e; i++) kept.push([i, shown[i - 1]]);
  }
  return { kept, total: shown.length + dropped };
}

/** Mirrors `render_triage_failure`. */
export function renderTriageFailure(why: string): string {
  return (
    `result withheld — triage failed (${why}). This is a harness failure, not a ` +
    `failure of your call: the call ran to completion and its side effects stand, ` +
    `and its raw output is in the session log — only the step that curates it for ` +
    `you did not complete. Re-issue the call if its output is still needed and ` +
    `running it again is safe.`
  );
}

export interface WireViewText {
  text: string;
  isError: boolean;
}

/** The line an interrupted call projects in place of its output — a mirror of
 * `render_interruption` (src/project/render.rs), so the operator reads what the
 * model read. */
export function renderInterruption(interruption: Interruption): string {
  const by = interruption.by;
  const session = by === undefined ? null : /^session\[(.+)\]$/.exec(by);
  const who =
    by === undefined || by === "web"
      ? "the operator"
      : session !== null
        ? `${session[1]} (agent)`
        : by;
  if (interruption.after_ms === undefined) {
    return `[interrupted by ${who} before its result was read]`;
  }
  const secs = Math.floor(interruption.after_ms / 1000);
  const after =
    secs < 60 ? `${secs}s` : `${Math.floor(secs / 60)}m ${secs % 60}s`;
  const what =
    interruption.killed === true
      ? "the command was killed"
      : "the call finished";
  return `[interrupted by ${who} after ${after}: ${what}, and its output was withheld]`;
}

/** The wire-projected view of a tool result — what the model saw (minus the
 * `[seq]` addressing prefix), mirroring `tool_result_block`'s per-state
 * dispatch. `null` when the wire state is `Raw`: raw *is* the view, and the
 * caller needs no toggle. A `Pending` result has no wire view yet; its raw
 * is all there is. */
export function wireView(raw: RawResult, wire: WireState): WireViewText | null {
  if (wire === "Raw" || wire === "Pending") return null;
  if (wire === "Elided") return { text: "[elided]", isError: false };
  if ("Interrupted" in wire) {
    return {
      text: renderInterruption(wire.Interrupted),
      isError: wire.Interrupted.killed === true,
    };
  }
  if ("Triaged" in wire) {
    return {
      text: renderTriaged(
        raw.content,
        wire.Triaged.keep,
        wire.Triaged.note,
        wire.Triaged.intent_feedback,
      ),
      isError: raw.is_error,
    };
  }
  return { text: renderTriageFailure(wire.TriageFailed.why), isError: true };
}
