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
  if (wire === "Raw") {
    const body = richRaw(tool, raw);
    return {
      body,
      stateWord: "raw",
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

/** How wide a rendered value may be and still sit inside the signature. Past
 * it the argument is elided to its shape and rendered as a block below, which
 * is what keeps the signature the call's identity rather than its payload. */
const INLINE_BUDGET = 48;

/** One argument as it reads inside the signature. */
export interface SigArg {
  key: string;
  /** The value: `"serde"`, `4`, `true` — or, when it did not fit, the shape
   * standing in for it (`505 lines`, `{3 keys}`). */
  text: string;
  /** True when `text` is a shape rather than the value. */
  elided: boolean;
}

/** A value too big for the signature, rendered below it. */
export interface CallBody {
  /** The argument this is the value of; null for bash's bare command. */
  key: string | null;
  lang: string | null;
  text: string;
}

export interface CallView {
  /** The call's `intent` — leads the call entry, as in the TUI. */
  intent: string | null;
  /** The `<server>` half of a namespaced name, shown dimmed ahead of the dot;
   * null for a built-in. Built-in versus mounted-server is the
   * `open_world_hint` distinction — whether the call could reach outside the
   * session — so it carries a standing visual rather than a hover. */
  server: string | null;
  /** The `<tool>` half, which for a built-in is the whole name. */
  tool: string;
  /** The signature's arguments, in input order with the elided ones last.
   * Empty renders `check_inbox()`; null means the call renders bare. */
  args: SigArg[] | null;
  /** bash's single-line command, rendered after the name unquoted. Set only
   * when `args` is null. */
  target: string | null;
  bodies: CallBody[];
}

/** The signature as one line of text, for surfaces with no room for spans (the
 * gate cue). The rendered head is the same shape in markup. */
export function callSignature(view: CallView): string {
  const name = view.server === null ? view.tool : `${view.server}.${view.tool}`;
  if (view.args === null)
    return view.target === null ? name : `${name} · ${view.target}`;
  return `${name}(${view.args.map((a) => `${a.key}: ${a.text}`).join(", ")})`;
}

/**
 * Every call as a pseudocode signature — total, schema-free, and therefore
 * correct for a tool this client has never heard of, which matters because MCP
 * servers are user-mountable and their argument shapes are arbitrary.
 *
 * One rule decides inline versus body for every tool: an argument sits in the
 * signature when it is scalar, single-line, and inside [`INLINE_BUDGET`];
 * otherwise it is elided to its shape there and rendered as a block below.
 * "Which argument is the body" is decided by what did not fit, not by a per-tool
 * table — so a third-party tool shaped like a file write needs no special
 * knowledge. A tool with no arguments renders `check_inbox()`, with no block.
 *
 * Two cases stay special, and only two. `bash` renders its command bare: a
 * shell command quoted into `bash(command: "…")` reads worse than the command.
 * The edit family keeps its arguments in the signature but emits no body — the
 * diff rendered below the head is the representation there (docs/web-client.md),
 * and a body would show the change twice; the inspector's args tab
 * ([`callArgs`]) is where the whole input is asked for.
 */
export function callView(name: string, input: unknown): CallView {
  const base = { intent: null as string | null, ...splitToolName(name) };
  if (typeof input !== "object" || input === null) {
    // Not an object at all: nothing to name, so the value itself is the body.
    return {
      ...base,
      args: [],
      target: null,
      bodies:
        input === undefined
          ? []
          : [{ key: null, lang: "json", text: pretty(input) }],
    };
  }
  const obj = input as Record<string, unknown>;
  const str = (key: string) =>
    typeof obj[key] === "string" ? (obj[key] as string) : null;
  base.intent = str("intent");

  if (name === "bash") {
    const command = str("command");
    if (command !== null) {
      return command.includes("\n")
        ? {
            ...base,
            args: null,
            target: null,
            bodies: [{ key: null, lang: "bash", text: command }],
          }
        : { ...base, args: null, target: command, bodies: [] };
    }
  }

  const lang = blobLang(name, obj);
  const suppressBodies = isDiffTool(name);
  const args: SigArg[] = [];
  const bodies: CallBody[] = [];
  for (const [key, value] of Object.entries(obj)) {
    // `intent` is already the call's leading line; repeating it in the
    // signature showed the same sentence twice.
    if (key === "intent") continue;
    const inline = inlineValue(value);
    if (inline !== null) {
      args.push({ key, text: inline, elided: false });
      continue;
    }
    args.push({ key, text: shapeOf(value), elided: true });
    if (suppressBodies) continue;
    bodies.push(
      typeof value === "string"
        ? { key, lang, text: value }
        : { key, lang: "json", text: pretty(value) },
    );
  }
  // Inline arguments first — they are the call's identity, and a shape between
  // two of them pushes "what did this touch" out of the first line. Stable, so
  // the input's own order still shows within each group.
  args.sort((a, b) => Number(a.elided) - Number(b.elided));
  return { ...base, args, target: null, bodies };
}

/** The value as it reads inside the signature, or null when it does not fit:
 * strings quoted, every other scalar bare, a collection as compact JSON while
 * that stays inside the budget. */
function inlineValue(value: unknown): string | null {
  if (typeof value === "string")
    return value.includes("\n") || value.length > INLINE_BUDGET
      ? null
      : JSON.stringify(value);
  if (value === null || typeof value === "number" || typeof value === "boolean")
    return String(value);
  let compact: string;
  try {
    compact = JSON.stringify(value) ?? "";
  } catch {
    return null;
  }
  return compact.length > 0 &&
    compact.length <= INLINE_BUDGET &&
    !compact.includes("\n")
    ? compact
    : null;
}

const plural = (n: number, noun: string) => `${n} ${noun}${n === 1 ? "" : "s"}`;

/** What stands in for an elided value: what a reader needs to know is how much
 * of it there is, and of what. */
function shapeOf(value: unknown): string {
  if (typeof value === "string") {
    const lines = rustLines(value).length;
    return lines > 1 ? plural(lines, "line") : plural(value.length, "char");
  }
  if (Array.isArray(value)) return `[${plural(value.length, "item")}]`;
  if (value !== null && typeof value === "object")
    return `{${plural(Object.keys(value).length, "key")}}`;
  return "…";
}

/** The language a call's string blobs are in: what it declared, else the
 * extension of the path it operates on. Shared by the signature's bodies and
 * the inspector's args tab. */
function blobLang(name: string, obj: Record<string, unknown>): string | null {
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
 * Every argument of a call, for the inspector's args tab (backlog/web/…).
 *
 * Separate from [`callView`] on purpose. `callView.body` answers "what does the
 * transcript header need beyond its summary", and for the edit family the
 * answer is nothing — the diff below is the representation, so dumping the
 * (verbose) input there would show the change twice. The args tab asks a
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

function pretty(input: unknown): string {
  try {
    return JSON.stringify(input, null, 2) ?? String(input);
  } catch {
    return String(input);
  }
}
