import { viewablePath } from "../../session/touchedFiles.ts";
import type { WireState } from "../../api/types.ts";
import { readMarker } from "../../render/parse.ts";
import { rustLines, normalize, PEEK_LINE_CAP } from "../../render/triage.ts";
import type { LineRange } from "../lineRanges.ts";
export interface CapturedFile {
  path: string;
  text: string;
  retained: LineRange[];
  header: string;
  startLine: number;
}
export function capturedReads(
  content: string,
  wire: WireState,
): CapturedFile[] {
  const rows = rustLines(content);
  const ranges =
    typeof wire === "object" && "Triaged" in wire
      ? normalize(
          wire.Triaged.keep === "All"
            ? [[1, PEEK_LINE_CAP]]
            : wire.Triaged.keep === "None"
              ? []
              : wire.Triaged.keep.Ranges,
          Math.min(rows.length, PEEK_LINE_CAP),
        )
      : wire === "Elided"
        ? []
        : [[1, rows.length]];
  const kept = (n: number) => ranges.some(([a, b]) => n >= a && n <= b);
  const files: CapturedFile[] = [];
  let active: {
    path: string;
    header: string;
    startLine: number;
    lines: string[];
    kept: LineRange[];
  } | null = null;
  for (const [i, line] of rows.entries()) {
    const marker = readMarker(line);
    if (marker) {
      if (active)
        files.push({
          path: active.path,
          header: active.header,
          text: active.lines.join("\n"),
          retained: active.kept,
          startLine: active.startLine,
        });
      active =
        marker.path !== null && !line.includes(" BEGIN ERROR ")
          ? {
              path: marker.path,
              header: line,
              lines: [],
              kept: [],
              startLine: Number(line.match(/LINES: (\d+)/)?.[1] ?? 1),
            }
          : null;
    } else if (active) {
      const expected = active.startLine + active.lines.length;
      active.lines.push(
        active.header.includes(", LINES:") && line.startsWith(`${expected}\t`)
          ? line.slice(String(expected).length + 1)
          : line,
      );
      if (kept(i + 1))
        active.kept.push([active.lines.length, active.lines.length]);
    }
  }
  if (active) {
    const file = active;
    files.push({
      path: file.path,
      header: file.header,
      text: file.lines.join("\n"),
      retained: file.kept,
      startLine: file.startLine,
    });
  }
  return files;
}
export function fileOperation(name: string): boolean {
  return ["read", "write", "edit", "retarget_edit", "undo_edit"].includes(name);
}

/** Range suffixes are removed only when the canonical capture confirms the
 * resulting path. Real filenames containing colons retain their spelling. */
export function currentReadPath(
  file: CapturedFile,
  input: unknown,
): string | null {
  const paths =
    typeof input === "object" && input !== null && "paths" in input
      ? input.paths
      : null;
  if (!Array.isArray(paths)) return null;
  for (const value of paths) {
    if (typeof value !== "string") continue;
    const candidates = [
      value,
      value.replace(
        /(?::(?:L?-?\d+)?(?:[-:.]{1,2}L?-?\d+)?|#L\d+(?:-L\d+)?)$/,
        "",
      ),
    ];
    for (const candidate of candidates) {
      const path = viewablePath(candidate);
      if (
        path !== null &&
        (file.path === path || file.path.endsWith(`/${path}`))
      )
        return path;
    }
  }
  return null;
}
