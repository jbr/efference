// What a call's arguments are, before any width is known (docs/web-client.md
// "A call reads as a signature"): which sit in the signature, which are
// content rendered below it, and which of a built-in's arguments go
// unlabelled. How the signature then breaks across lines at a given width is
// `signatureLayout.ts`; this half is width-free, so every surface that names a
// call — the transcript, the inspector, the gate cue, a brief row — agrees on
// what the call's arguments are.

import {
  blobLang,
  isDiffTool,
  pretty,
  splitToolName,
} from "../render/parse.ts";

/** One argument as the signature holds it. */
export interface SigArg {
  /** The input key the value came from — what a chip is looked up by
   * (chipTable.ts), whether or not the signature shows it. */
  key: string;
  /** Shown ahead of the value (`from_line: 3`), or null for a built-in's
   * primary argument, which reads positionally (`read("a.rs")`). */
  label: string | null;
  /** The value itself. Never a multi-line string, nor a collection holding
   * one: those are content, and render below as a [`CallBody`]. */
  value: unknown;
}

/** An argument rendered below the signature as content rather than in it. */
export interface CallBody {
  /** The argument this is the value of; null for bash's bare command. */
  key: string | null;
  lang: string | null;
  text: string;
  /** Rendered as prose rather than as code: a body that is a message, like a
   * hand-off's brief, not a value. */
  prose?: boolean;
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
  /** The signature's arguments, positional first. Empty renders
   * `check_inbox()`; null means the call renders bare. */
  args: SigArg[] | null;
  /** bash's single-line command, rendered unquoted in place of the name and
   * arguments. Set only when `args` is null. */
  target: string | null;
  /** bash's way of running: whether the call asked to be backgrounded from
   * the start, and the seconds after which it would move there on its own.
   * Null for every other tool. */
  shell: { background: boolean; backgroundAfter: number | null } | null;
  bodies: CallBody[];
}

/**
 * The built-ins whose primary argument reads without its key: the one thing
 * the call acts on, which its name already implies — `read("a.rs", "b.rs")`,
 * not `read(paths: ["a.rs", "b.rs"])`. An array here spreads into one
 * positional argument per item. Keyed by wire name, so a mounted server's
 * same-named tool keeps its labels: a mounted tool's argument shapes are its
 * own, and only a built-in's are known.
 */
const POSITIONAL: Record<string, string> = {
  read: "paths",
  list: "path",
  search: "pattern",
  write: "path",
  edit: "file_path",
  undo_edit: "file_path",
  retarget_edit: "file_path",
  find_anchor: "file_path",
  read_image: "path",
  read_process: "handle",
  kill_process: "handle",
  watch_process: "handle",
};

/** Whether a value is content rather than an argument: a string with a line
 * break in it, anywhere inside the value. A line break is what a file's
 * contents, a heredoc, or a prose message have and an identifier never does. */
function isContent(value: unknown): boolean {
  if (typeof value === "string") return value.includes("\n");
  if (Array.isArray(value)) return value.some(isContent);
  if (value !== null && typeof value === "object")
    return Object.values(value).some(isContent);
  return false;
}

const isScalar = (value: unknown) =>
  value === null || typeof value !== "object";

/** A content value as the block below renders it: a string verbatim, with its
 * line breaks as line breaks; anything else pretty-printed. */
export function bodyOf(
  key: string,
  value: unknown,
  lang: string | null,
): CallBody {
  return typeof value === "string"
    ? { key, lang, text: value }
    : { key, lang: "json", text: pretty(value) };
}

/**
 * Every call as a pseudocode signature — total, and correct for a tool this
 * client has never heard of, which matters because MCP servers are
 * user-mountable and their argument shapes are arbitrary.
 *
 * A multi-line value is content: it leaves the signature entirely and renders
 * below it under its key. Every other argument sits in the signature — whether
 * one is too long to sit there sensibly depends on the width, which is
 * `signatureLayout.ts`'s decision. A tool with no arguments renders
 * `check_inbox()`, with no block.
 *
 * Two cases stay special. `bash` renders its command bare, after a prompt
 * rather than its name: a shell command quoted into `bash(command: "…")` reads
 * worse than the command, and the prompt says "shell" without a word. Its
 * backgrounding rides along as `shell`. The edit family emits no bodies —
 * the diff rendered below the head is the representation there
 * (docs/web-client.md), and a body would show the change twice; the
 * inspector's args tab (`callArgs`) is where the whole input is asked for.
 */
export function callView(name: string, input: unknown): CallView {
  const base = { intent: null as string | null, ...splitToolName(name) };
  if (typeof input !== "object" || input === null) {
    // Not an object at all: nothing to name, so the value itself is the body.
    return {
      ...base,
      args: [],
      target: null,
      shell: null,
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

  // A hand-off's brief is a message to the session that takes over
  // (docs/tools.md): labelled with whom it is for and read as prose, so it is
  // not mistaken for the agent addressing the operator.
  const brief = name === "agents__hand_off" ? str("brief") : null;
  if (brief !== null) {
    return {
      ...base,
      args: [],
      target: null,
      shell: null,
      bodies: [
        { key: "to the new session", lang: null, text: brief, prose: true },
      ],
    };
  }

  if (name === "bash") {
    const command = str("command");
    if (command !== null) {
      const shell = {
        background: obj.background === true,
        backgroundAfter:
          typeof obj.background_after === "number"
            ? obj.background_after
            : null,
      };
      return command.includes("\n")
        ? {
            ...base,
            args: null,
            target: null,
            shell,
            bodies: [{ key: null, lang: "bash", text: command }],
          }
        : { ...base, args: null, target: command, shell, bodies: [] };
    }
  }

  const lang = blobLang(name, obj);
  const primary = POSITIONAL[name];
  const positional: SigArg[] = [];
  const labelled: SigArg[] = [];
  const bodies: CallBody[] = [];
  for (const [key, value] of Object.entries(obj)) {
    // `intent` is already the call's leading line; repeating it in the
    // signature showed the same sentence twice.
    if (key === "intent") continue;
    if (isContent(value)) {
      if (!isDiffTool(name)) bodies.push(bodyOf(key, value, lang));
      continue;
    }
    if (key !== primary) {
      labelled.push({ key, label: key, value });
    } else if (
      Array.isArray(value) &&
      value.length > 0 &&
      value.every(isScalar)
    ) {
      for (const item of value)
        positional.push({ key, label: null, value: item });
    } else {
      positional.push({ key, label: null, value });
    }
  }
  return {
    ...base,
    args: [...positional, ...labelled],
    target: null,
    shell: null,
    bodies,
  };
}
