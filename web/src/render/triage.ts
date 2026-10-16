// ⚠ CORDON — a deliberate, temporary duplicate of the daemon's projection
// renderer (`src/project/render.rs`; docs/web-client.md "The cordon").
// The triaged view must show what the model saw, and until tool results
// are structured (audience-tagged content blocks, or per-tool result
// schemas), the only client-side source is a mirror of the Rust renderer.
// Everything under `web/src/render/` is planned deletion: when structured
// output lands, drop the directory whole. Ports are literal — same names,
// same order, same strings — so divergence is findable by diffing against
// render.rs; the tests mirror `src/project/test/triaged.rs` case for case.

import type { Keep, RawResult, WireState } from "../api/types.ts";

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

/** Mirrors `render_hunks`. */
export function renderHunks(
  lines: string[],
  ranges: Range[],
  note: string | null,
): string {
  const parts: string[] = [];
  for (const [start, end] of normalize(ranges, lines.length)) {
    parts.push(hunkHeader(start, end));
    for (let i = start; i <= end; i++) parts.push(lines[i - 1]);
  }
  const trimmed = note?.trim();
  if (trimmed) parts.push(`⟢ ${trimmed}`);
  if (parts.length === 0) parts.push("(nothing kept)");
  return parts.join("\n");
}

/** Mirrors `render_triaged`. */
export function renderTriaged(
  content: string,
  keep: Keep,
  note: string | null,
): string {
  const all = rustLines(content);
  const shown = all.slice(0, TRIAGE_LINE_CAP);
  const dropped = all.length - shown.length;
  const ranges: Range[] =
    keep === "All" ? [[1, shown.length]] : keep === "None" ? [] : keep.Ranges;
  let out = renderHunks(shown, ranges, note);
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
    `result withheld — triage failed (${why}). The call ran to completion and its ` +
    `side effects stand, but its output was not recorded. Re-issue the call if the ` +
    `output is still needed.`
  );
}

export interface WireViewText {
  text: string;
  isError: boolean;
}

/** The wire-projected view of a tool result — what the model saw (minus the
 * `[seq]` addressing prefix), mirroring `tool_result_block`'s per-state
 * dispatch. `null` when the wire state is `Raw`: raw *is* the view, and the
 * caller needs no toggle. A `Pending` result has no wire view yet; its raw
 * is all there is. */
export function wireView(raw: RawResult, wire: WireState): WireViewText | null {
  if (wire === "Raw" || wire === "Pending") return null;
  if (wire === "Elided") return { text: "[elided]", isError: false };
  if ("Triaged" in wire) {
    return {
      text: renderTriaged(raw.content, wire.Triaged.keep, wire.Triaged.note),
      isError: raw.is_error,
    };
  }
  return { text: renderTriageFailure(wire.TriageFailed.why), isError: true };
}
