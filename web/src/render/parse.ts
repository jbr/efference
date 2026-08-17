// ⚠ CORDON — a literal port of the TUI's per-tool parsing and dispatch
// (`src/tui/projection/tools.rs`, plus the distilled-header composition in
// `src/tui/projection.rs`; docs/web-client.md "The cordon"). Same deletion
// covenant as distill.ts: structured tool output deletes this directory
// whole. Ports stay literal — same names, same classifications — so
// divergence is findable by diffing against tools.rs.

import type { RawResult, WireState } from "../api/types.ts";
import {
  distillateKept,
  renderTriageFailure,
  rustLines,
  type NumberedLine,
} from "./distill.ts";

export type DiffLineKind = "added" | "removed" | "hunk" | "context";
export interface DiffLine {
  kind: DiffLineKind;
  text: string;
}

/** The wire state's one-word header label (the TriageFailed case carries
 * its why). */
export const wireLabel = (wire: WireState) => {
  if (wire === "Raw") return "raw";
  if (wire === "Elided") return "elided";
  if ("Distilled" in wire) return "distilled";
  return `triage failed: ${wire.TriageFailed.why}`;
};

/** Mirrors `is_diff_tool`: the tools whose result carries `===DIFF===`. */
export const isDiffTool = (name: string) =>
  name === "edit" || name === "undo_edit" || name === "retarget_edit";

/** Mirrors `lang_for_path` — carried on code views as the hook the eventual
 * syntax highlighter plugs into; unstyled until then. */
export function langForPath(path: string): string | null {
  const ext = path.split(".").at(-1);
  switch (ext) {
    case "rs":
      return "rust";
    case "toml":
      return "toml";
    case "md":
    case "markdown":
      return "markdown";
    case "json":
      return "json";
    case "js":
    case "mjs":
    case "cjs":
      return "javascript";
    case "ts":
      return "typescript";
    case "py":
      return "python";
    case "sh":
    case "bash":
      return "bash";
    case "yaml":
    case "yml":
      return "yaml";
    case "html":
      return "html";
    case "css":
      return "css";
    default:
      return null;
  }
}

/** Mirrors `read_marker`: `{ path }` for a BEGIN sentinel, `{ path: null }`
 * for an END, `null` for ordinary file content. */
export function readMarker(line: string): { path: string | null } | null {
  if (!line.startsWith("==") || !line.endsWith("==")) return null;
  const inner = line.slice(2, -2);
  const begin = inner.split(" BEGIN ");
  if (begin.length > 1) {
    const rest = begin.slice(1).join(" BEGIN ");
    const path = (rest.split(", LENGTH:")[0] ?? rest).trim();
    return { path };
  }
  if (inner.includes(" END ")) return { path: null };
  return null;
}

/** Mirrors `read_rows`: the file lines of a `read` result, each numbered by
 * its 1-based position in the raw result (the verdict's coordinate system),
 * sentinel framing dropped. `null` when no marker is present. */
export function readRows(
  content: string,
): { lang: string | null; lines: NumberedLine[] } | null {
  let path: string | null = null;
  let sawMarker = false;
  const lines: NumberedLine[] = [];
  rustLines(content).forEach((line, idx) => {
    const marker = readMarker(line);
    if (marker) {
      sawMarker = true;
      if (marker.path !== null) path = marker.path;
    } else {
      lines.push([idx + 1, line]);
    }
  });
  if (!sawMarker) return null;
  return { lang: path === null ? null : langForPath(path), lines };
}

/** Mirrors `diff_rows`: the lines after `===DIFF===`, classified by their
 * leading ` `/`+`/`-`/`@`, numbered by raw position. Prose above the marker
 * carries no row; leading blank lines after it are skipped. `null` when the
 * marker is absent or the section is all blank. */
export function diffRows(content: string): [number, DiffLine][] | null {
  const lines = rustLines(content);
  const at = lines.findIndex((line) => line.includes("===DIFF==="));
  if (at === -1) return null;
  let start = at + 1;
  while (start < lines.length && lines[start] === "") start++;
  const rows: [number, DiffLine][] = [];
  for (let i = start; i < lines.length; i++) {
    const line = lines[i];
    const kind: DiffLineKind =
      line[0] === "+"
        ? "added"
        : line[0] === "-"
          ? "removed"
          : line[0] === "@"
            ? "hunk"
            : "context";
    rows.push([i + 1, { kind, text: line }]);
  }
  if (rows.every(([, line]) => line.text.trim() === "")) return null;
  return rows;
}

/** What a result body renders as; `lang` on code/numbered views is the
 * future highlighter's hook. */
export type ResultView =
  | { kind: "diff"; lines: DiffLine[] }
  | { kind: "code"; lang: string | null; text: string }
  | { kind: "numbered"; lang: string | null; lines: NumberedLine[] }
  | { kind: "text"; text: string };

export interface ResultRender {
  body: ResultView;
  /** The TUI's size half-label — "kept N of M lines" — distilled only. */
  sizeLabel: string | null;
  /** The distiller's note; true of the output whichever form is shown. */
  note: string | null;
  isError: boolean;
  /** The body is not verbatim raw — offer the raw audit expander. */
  rawDiffers: boolean;
}

/** Mirrors `result_body`'s raw-state dispatch: a diff tool's `===DIFF===`
 * classified, a `read` framing-stripped and language-tagged; errors and
 * unknown tools fall back to plain text. */
function richRaw(tool: string | null, raw: RawResult): ResultView {
  if (!raw.is_error && tool !== null) {
    if (isDiffTool(tool)) {
      const rows = diffRows(raw.content);
      if (rows) return { kind: "diff", lines: rows.map(([, l]) => l) };
    }
    if (tool === "read") {
      const rows = readRows(raw.content);
      if (rows) {
        return {
          kind: "code",
          lang: rows.lang,
          text: rows.lines.map(([, l]) => l).join("\n"),
        };
      }
    }
  }
  return { kind: "text", text: raw.content };
}

/** Mirrors `distilled_body`: the same tool-specific parse over the whole
 * raw, restricted to the rows whose line numbers the verdict kept; numbered
 * plain lines otherwise. */
function distilledBody(
  tool: string | null,
  raw: RawResult,
  kept: NumberedLine[],
): ResultView {
  const keeps = new Set(kept.map(([n]) => n));
  if (!raw.is_error && tool !== null) {
    if (isDiffTool(tool)) {
      const rows = diffRows(raw.content);
      const lines = rows?.filter(([n]) => keeps.has(n)).map(([, line]) => line);
      if (lines && lines.length > 0) return { kind: "diff", lines };
    }
    if (tool === "read") {
      const rows = readRows(raw.content);
      const lines = rows?.lines.filter(([n]) => keeps.has(n));
      if (lines && lines.length > 0) {
        return { kind: "numbered", lang: rows!.lang, lines };
      }
    }
  }
  return { kind: "numbered", lang: null, lines: kept };
}

/** The one dispatch a result block renders through, mirroring the TUI's
 * per-wire-state match (`src/tui/projection.rs`). */
export function resultRender(
  tool: string | null,
  raw: RawResult,
  wire: WireState,
): ResultRender {
  if (wire === "Raw") {
    const body = richRaw(tool, raw);
    return {
      body,
      sizeLabel: null,
      note: null,
      isError: raw.is_error,
      rawDiffers: body.kind !== "text",
    };
  }
  if (wire === "Elided") {
    return {
      body: { kind: "text", text: "[elided]" },
      sizeLabel: null,
      note: null,
      isError: false,
      rawDiffers: true,
    };
  }
  if ("Distilled" in wire) {
    const { keep, note } = wire.Distilled;
    const { kept, total } = distillateKept(raw.content, keep);
    return {
      body: distilledBody(tool, raw, kept),
      sizeLabel: `kept ${kept.length} of ${total} line${total === 1 ? "" : "s"}`,
      note,
      isError: raw.is_error,
      rawDiffers: true,
    };
  }
  return {
    body: { kind: "text", text: renderTriageFailure(wire.TriageFailed.why) },
    sizeLabel: null,
    note: null,
    isError: true,
    rawDiffers: true,
  };
}

export interface CallView {
  /** The call's `intent` — leads the call entry, as in the TUI. */
  intent: string | null;
  /** Inline summary spans for the `tool: name · …` header line. */
  summary: string | null;
  /** A body block — a multi-line command, file contents, or the JSON
   * envelope fallback for an unrecognized tool. */
  body: { lang: string | null; text: string } | null;
}

/** Mirrors `call_args`: known tools summarize their identifying argument
 * inline and show the substantial one as a block; unknown tools keep the
 * pretty-printed JSON envelope. `intent` rides beside every shape. */
export function callView(name: string, input: unknown): CallView {
  const obj =
    typeof input === "object" && input !== null
      ? (input as Record<string, unknown>)
      : {};
  const str = (key: string) =>
    typeof obj[key] === "string" ? (obj[key] as string) : null;
  const intent = str("intent");
  const view = (summary: string | null, body: CallView["body"]): CallView => ({
    intent,
    summary,
    body,
  });
  if (name === "bash") {
    const command = str("command");
    if (command !== null) {
      return command.includes("\n")
        ? view(null, { lang: "bash", text: command })
        : view(command, null);
    }
  }
  if (name === "read" && Array.isArray(obj.paths)) {
    return view(
      obj.paths.filter((p) => typeof p === "string").join(", "),
      null,
    );
  }
  if (name === "search") {
    const pattern = str("pattern");
    if (pattern !== null) {
      const path = str("path");
      return view(path === null ? pattern : `${pattern}  in  ${path}`, null);
    }
  }
  if (name === "list") {
    const path = str("path");
    if (path !== null) return view(path, null);
  }
  if (name === "write") {
    const path = str("path");
    if (path !== null) {
      let summary = path;
      for (const [key, label] of [
        ["append", " · append"],
        ["overwrite", " · overwrite"],
      ] as const) {
        if (obj[key] === true) summary += label;
      }
      const contents = str("contents");
      return view(
        summary,
        contents === null ? null : { lang: langForPath(path), text: contents },
      );
    }
  }
  if (isDiffTool(name)) {
    const path = str("file_path");
    const operation = str("operation");
    const summary = [path, operation].filter((s) => s !== null).join(" · ");
    return view(summary === "" ? null : summary, null);
  }
  return view(null, { lang: "json", text: pretty(input) });
}

function pretty(input: unknown): string {
  try {
    return JSON.stringify(input, null, 2) ?? String(input);
  } catch {
    return String(input);
  }
}
