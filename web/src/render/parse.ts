// ! CORDON — a literal port of the TUI's per-tool parsing and dispatch
// (`src/tui/projection/tools.rs`, plus the distilled-header composition in
// `src/tui/projection.rs`; docs/web-client.md "The cordon"). Same deletion
// covenant as distill.ts: structured tool output deletes this directory
// whole. Ports stay literal — same names, same classifications — so
// divergence is findable by diffing against tools.rs.

import type { DistillateLines, RawResult, WireState } from "../api/types.ts";
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
  if (wire === "Pending") return "awaiting triage";
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
  /** The wire state's word on a result head ("raw", "distilled", "elided").
   * Null when the state is not worth naming: triage kept every line, so the
   * head presents like any whole result and nothing may imply the operator
   * sees less than the model did. The inspector's wire row keeps the
   * unconditional `wireLabel` — there it is metadata, not noise. */
  stateWord: string | null;
  /** The TUI's size half-label — "kept N of M lines" — distilled only, and
   * null alongside a null `stateWord` when nothing was dropped. */
  sizeLabel: string | null;
  /** The distiller's note; true of the output whichever form is shown. */
  note: string | null;
  /** "16 KiB of 256 KiB shown" when the control channel truncated this
   * result's `content` (docs/control-api.md), else null. Mirrors the marker
   * `result_entry` puts in the TUI's result header, so a body that stops
   * mid-output is never read as the whole of what the tool returned. */
  elisionLabel: string | null;
  isError: boolean;
}

/** Mirrors `byte_size` (`src/tui/projection.rs`): bytes below a kibibyte,
 * whole KiB above it. A gloss, not an accounting figure. */
export const byteSize = (bytes: number) =>
  bytes < 1024 ? `${bytes} B` : `${Math.floor(bytes / 1024)} KiB`;

/** The elision marker for a result, from the tag the view carries — never
 * from measuring `content`, which is only ever the prefix that arrived. */
export function elisionLabel(raw: RawResult): string | null {
  const elided = raw.content_elided;
  if (elided === undefined) return null;
  return `${byteSize(elided.kept_bytes)} of ${byteSize(elided.total_bytes)} shown`;
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
  /** The daemon's line arithmetic (docs/control-api.md). Authoritative when
   * present: `raw.content` may be a truncated prefix, over which the local
   * count reads "kept 0 of 198" for a result whose kept lines all lie past
   * it. Optional so a client of an older daemon still renders. */
  distillate?: DistillateLines,
): ResultRender {
  if (wire === "Raw" || wire === "Pending") {
    const body = richRaw(tool, raw);
    return {
      body,
      // A pending result is its raw output, labelled as not yet triaged
      // (the TUI's "awaiting triage"); the verdict re-renders the slot.
      stateWord: wire === "Pending" ? "awaiting triage" : "raw",
      sizeLabel: null,
      note: null,
      elisionLabel: elisionLabel(raw),
      isError: raw.is_error,
    };
  }
  if (wire === "Elided") {
    return {
      body: { kind: "text", text: "[elided]" },
      stateWord: "elided",
      sizeLabel: null,
      note: null,
      elisionLabel: null,
      isError: false,
    };
  }
  if ("Distilled" in wire) {
    const { keep, note } = wire.Distilled;
    const local = distillateKept(raw.content, keep);
    const kept = local.kept;
    const keptCount = distillate?.kept_lines ?? kept.length;
    const total = distillate?.total_lines ?? local.total;
    // When triage kept everything, "distilled · kept n of n lines" says only
    // that triage ran — a third of real results. The view is everything the
    // model saw, so the head presents like any whole result.
    const complete = keptCount === total;
    // Kept lines that lie past the loaded prefix have no rows to show. The body
    // says so rather than presenting as a verdict that kept nothing.
    const beyond = raw.content_elided !== undefined && keptCount > kept.length;
    return {
      body:
        beyond && kept.length === 0
          ? {
              kind: "text",
              text: `the ${keptCount} kept line${keptCount === 1 ? "" : "s"} lie past the loaded prefix`,
            }
          : distilledBody(tool, raw, kept),
      stateWord: complete ? null : "distilled",
      sizeLabel: complete
        ? null
        : `kept ${keptCount} of ${total} line${total === 1 ? "" : "s"}`,
      note,
      elisionLabel: elisionLabel(raw),
      isError: raw.is_error,
    };
  }
  return {
    body: { kind: "text", text: renderTriageFailure(wire.TriageFailed.why) },
    stateWord: `triage failed: ${wire.TriageFailed.why}`,
    sizeLabel: null,
    note: null,
    elisionLabel: null,
    isError: true,
  };
}

/** Mirrors `mcp::display_name`/`mcp::namespace_of` (src/mcp.rs): the namespace
 * boundary is the *first* `__`, so a server's own `__`-bearing tool name keeps
 * it. A bare built-in name has no server half — it is not given an `efference.`
 * prefix on either client. Display only: the dotted form is not a name anything
 * can be looked up by. */
export function splitToolName(wireName: string): {
  server: string | null;
  tool: string;
} {
  const i = wireName.indexOf("__");
  return i === -1
    ? { server: null, tool: wireName }
    : { server: wireName.slice(0, i), tool: wireName.slice(i + 2) };
}

/** The language a call's string blobs are in: what it declared, else the
 * extension of the path it operates on. Shared by the signature's bodies and
 * the inspector's args tab. */
export function blobLang(
  name: string,
  obj: Record<string, unknown>,
): string | null {
  // A bash command is shell whatever file it touches.
  if (name === "bash") return "bash";
  const str = (key: string) =>
    typeof obj[key] === "string" ? (obj[key] as string) : null;
  const declared = str("language");
  const path = str("file_path") ?? str("path");
  return declared ?? (path === null ? null : langForPath(path));
}

/** One argument as the inspector's args tab shows it: a scalar renders inline,
 * a multi-line string as a code block. */
export interface ArgField {
  key: string;
  /** Set when the value fits on one line — a path, a flag, a number. */
  scalar: string | null;
  /** Set when the value is multi-line: source, a heredoc, a nested envelope. */
  code: { lang: string | null; text: string } | null;
}

/**
 * Every argument of a call, for the inspector's args tab (docs/web-client.md).
 *
 * Separate from the signature (`ui/signature.ts`) on purpose. The signature's
 * bodies answer "what does the transcript header need beyond its summary",
 * and for the edit family the answer is nothing — the diff below is the
 * representation, so dumping the (verbose) input there would show the change
 * twice. The args tab asks a
 * different question: what was asked. Serving both from one value is what made
 * the tab claim "no body — the header says it all" while dropping `anchor` and
 * `content`, the substance of the call.
 *
 * Multi-line strings come back as code rather than escaped JSON scalars. A
 * pretty-print puts a source blob on one line with literal `\n`s and no
 * highlighting, which is the least readable form of the one thing worth
 * reading. The language is the call's own `language` when it names one, else
 * the file extension it is operating on.
 */
export function callArgs(name: string, input: unknown): ArgField[] {
  if (typeof input !== "object" || input === null) {
    return input === undefined
      ? []
      : [{ key: "input", scalar: String(input), code: null }];
  }
  const obj = input as Record<string, unknown>;
  // `intent` is already the call's leading line; repeating it here showed the
  // same sentence twice.
  const lang = blobLang(name, obj);
  return (
    Object.entries(obj)
      .filter(([key]) => key !== "intent")
      .map(([key, value]): ArgField => {
        if (typeof value === "string") {
          if (!value.includes("\n")) return { key, scalar: value, code: null };
          return { key, scalar: null, code: { lang, text: value } };
        }
        if (value === null || typeof value !== "object") {
          return { key, scalar: String(value), code: null };
        }
        const text = pretty(value);
        return text.includes("\n")
          ? { key, scalar: null, code: { lang: "json", text } }
          : { key, scalar: text, code: null };
      })
      // Scalars first, whatever order the input serialized in: they are the
      // call's identity — which file, which operation — and a blob above them
      // pushes the answer to "what did this touch" off the top of the tab.
      // Stable within each group, so the input's own order still shows.
      .sort((a, b) => Number(a.code !== null) - Number(b.code !== null))
  );
}

export function pretty(input: unknown): string {
  try {
    return JSON.stringify(input, null, 2) ?? String(input);
  } catch {
    return String(input);
  }
}
