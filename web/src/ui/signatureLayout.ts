// A call's signature laid out at a width (docs/web-client.md "A call reads as
// a signature"). Pure: a view, a column count, and a chip allowance in; rows
// of tokens out. The column count comes from `columns.ts`; the view from
// `signature.ts`.
//
// The signature is set in the monospace face, so a width is a count of
// characters and fitting is arithmetic — no DOM is measured here. The rules
// are the pretty-printer's: a call that fits on one line is one line; one that
// does not breaks after `(` with an argument per line, and an argument whose
// value is a collection that still does not fit breaks the same way inside.
// A single value longer than a few whole lines is not an argument a reader
// scans, so it leaves the signature and renders below as content.
//
// Wide characters (CJK, emoji) count as one column each, so a line holding
// them can overrun its budget; the browser's own wrapping (`.call-sig`'s
// `overflow-wrap`) is the fallback, never a clipped line.

import type { CallView, SigArg } from "./signature.ts";

/** One run of text in the signature, with the role that colors it. */
export interface SigToken {
  kind: "server" | "tool" | "sep" | "key" | "value" | "target" | "tag";
  text: string;
  /** Set on the value of a top-level scalar argument — the only thing a chip
   * can attach to. */
  arg?: SigArg;
}

/** One line of a broken signature, indented by `indent` columns. */
export interface SigRow {
  indent: number;
  tokens: SigToken[];
}

export interface SigLayout {
  rows: SigRow[];
  /** A shell command: the head reads as a prompt, not a call (CallHead). */
  shell: boolean;
  /** True when the signature broke across rows. */
  broken: boolean;
  /** The arguments too long to sit in the signature at this width. They are
   * absent from `rows`, and render below it as content. */
  pulled: SigArg[];
}

/** Per-level indent of a broken signature, in columns. */
const INDENT = 2;

/** How many whole lines one value may take in the signature before it is
 * content rather than an argument. */
const PULL_LINES = 3;

/** Below this many columns the pull threshold stops shrinking: a phone's
 * column is narrow, and a path that wraps twice there is still a path. */
const MIN_PULL_COLUMNS = 32;

const IDENTIFIER = /^[A-Za-z_$][\w$]*$/;

/** A value as it reads flat: strings quoted (their escapes are what keeps an
 * embedded quote or comma unambiguous), other scalars bare, collections in the
 * signature's own `key: value` register. */
export function flatValue(value: unknown): string {
  if (typeof value === "string") return JSON.stringify(value);
  if (value === undefined) return "undefined";
  if (value === null || typeof value !== "object") return String(value);
  if (Array.isArray(value)) return `[${value.map(flatValue).join(", ")}]`;
  const entries = Object.entries(value);
  if (entries.length === 0) return "{}";
  return `{ ${entries.map(([k, v]) => `${keyText(k)}: ${flatValue(v)}`).join(", ")} }`;
}

const keyText = (key: string) =>
  IDENTIFIER.test(key) ? key : JSON.stringify(key);

/** Columns a string occupies. Code points, so a surrogate pair is one. */
const width = (text: string) => [...text].length;

function flatArg(arg: SigArg): string {
  const value = flatValue(arg.value);
  return arg.label === null ? value : `${arg.label}: ${value}`;
}

function nameTokens(view: CallView): SigToken[] {
  return [
    ...(view.server === null
      ? []
      : [{ kind: "server" as const, text: `${view.server}.` }]),
    { kind: "tool", text: view.tool },
  ];
}

/** An argument's tokens, flat. The value carries the argument when it is a
 * scalar, so a chip can replace it. */
function argTokens(arg: SigArg): SigToken[] {
  const value: SigToken = {
    kind: "value",
    text: flatValue(arg.value),
    ...(arg.value === null || typeof arg.value !== "object" ? { arg } : {}),
  };
  return arg.label === null
    ? [value]
    : [{ kind: "key", text: arg.label }, { kind: "sep", text: ": " }, value];
}

/** How a shell command runs, when it is not the default of blocking until it
 * exits: backgrounded from the start, or backgrounded if it outlasts the
 * seconds it named. */
function shellTags(shell: NonNullable<CallView["shell"]>): string[] {
  if (shell.background) return ["background"];
  if (shell.backgroundAfter !== null)
    return [`background after ${shell.backgroundAfter}s`];
  return [];
}

/** The signature on one line of text, for surfaces with no room for spans
 * (the gate cue). */
export function flatSignature(view: CallView): string {
  const name = view.server === null ? view.tool : `${view.server}.${view.tool}`;
  if (view.shell !== null) {
    const command =
      view.target ?? `${view.bodies[0]?.text.split("\n")[0] ?? ""} …`;
    return `› ${command}`;
  }
  if (view.args === null) return name;
  return `${name}(${view.args.map(flatArg).join(", ")})`;
}

/**
 * Lay the signature out in `columns`, or flat when the width is not yet known
 * (null — the first render, before `columns.ts` has measured). `chipExtra` is
 * the width a chip adds to the value it rides on (the path chip's ` ⤢`), so a
 * chipped argument is budgeted as it renders.
 */
export function layoutSignature(
  view: CallView,
  columns: number | null,
  chipExtra: (arg: SigArg) => number = () => 0,
): SigLayout {
  const name = nameTokens(view);
  if (view.shell !== null) {
    // bash: the command is verbatim and wraps as text; there is nothing to
    // break it at that would read better than its own spaces. A multi-line
    // command is a body below, so the head holds only its tags, if any.
    const tokens: SigToken[] = [
      ...(view.target === null
        ? []
        : [{ kind: "target" as const, text: view.target }]),
      ...shellTags(view.shell).map((text) => ({ kind: "tag" as const, text })),
    ];
    return {
      rows: tokens.length === 0 ? [] : [{ indent: 0, tokens }],
      shell: true,
      broken: false,
      pulled: [],
    };
  }
  if (view.args === null) {
    // The bare name: a call whose input has not started arriving.
    return {
      rows: [{ indent: 0, tokens: name }],
      shell: false,
      broken: false,
      pulled: [],
    };
  }

  const argWidth = (arg: SigArg) => width(flatArg(arg)) + chipExtra(arg);
  const limit =
    columns === null
      ? Infinity
      : PULL_LINES * Math.max(columns - INDENT, MIN_PULL_COLUMNS);
  const kept = view.args.filter((arg) => argWidth(arg) <= limit);
  const pulled = view.args.filter((arg) => argWidth(arg) > limit);

  const nameWidth = name.reduce((n, t) => n + width(t.text), 0);
  const flatWidth =
    nameWidth +
    2 +
    kept.reduce((n, arg) => n + argWidth(arg), 0) +
    2 * Math.max(kept.length - 1, 0);
  // A lone scalar argument stays hugged by its parentheses: breaking around
  // it adds two rows and the value wraps as text either way.
  const hugged =
    kept.length === 1 &&
    (kept[0].value === null || typeof kept[0].value !== "object");
  if (columns === null || flatWidth <= columns || kept.length === 0 || hugged) {
    const tokens: SigToken[] = [...name, { kind: "sep", text: "(" }];
    kept.forEach((arg, i) => {
      if (i > 0) tokens.push({ kind: "sep", text: ", " });
      tokens.push(...argTokens(arg));
    });
    tokens.push({ kind: "sep", text: ")" });
    return {
      rows: [{ indent: 0, tokens }],
      shell: false,
      broken: false,
      pulled,
    };
  }

  const rows: SigRow[] = [
    { indent: 0, tokens: [...name, { kind: "sep", text: "(" }] },
  ];
  kept.forEach((arg, i) => {
    rows.push(
      ...argRows(
        arg,
        INDENT,
        columns,
        i < kept.length - 1 ? "," : "",
        chipExtra,
      ),
    );
  });
  rows.push({ indent: 0, tokens: [{ kind: "sep", text: ")" }] });
  return { rows, shell: false, broken: true, pulled };
}

/** One argument of a broken signature: a row when it fits at its indent (or
 * is a scalar, which has nowhere to break but its own text), else its
 * collection opened across rows, each member laid out the same way. */
function argRows(
  arg: SigArg,
  indent: number,
  columns: number,
  trailer: string,
  chipExtra: (arg: SigArg) => number,
): SigRow[] {
  const tail: SigToken[] =
    trailer === "" ? [] : [{ kind: "sep", text: trailer }];
  const fits =
    indent + width(flatArg(arg)) + chipExtra(arg) + trailer.length <= columns;
  const value = arg.value;
  const members =
    Array.isArray(value) && value.length > 0
      ? value.map((item) => ({ key: arg.key, label: null, value: item }))
      : value !== null &&
          typeof value === "object" &&
          Object.keys(value).length > 0
        ? Object.entries(value).map(([k, v]) => ({
            key: arg.key,
            label: keyText(k),
            value: v,
          }))
        : null;
  if (fits || members === null) {
    return [{ indent, tokens: [...argTokens(arg), ...tail] }];
  }
  const [open, close] = Array.isArray(value) ? ["[", "]"] : ["{", "}"];
  const head: SigToken[] =
    arg.label === null
      ? [{ kind: "sep", text: open }]
      : [
          { kind: "key", text: arg.label },
          { kind: "sep", text: ": " },
          { kind: "sep", text: open },
        ];
  // Members are not the call's own arguments, so they carry no chip.
  const noChip = () => 0;
  return [
    { indent, tokens: head },
    ...members.flatMap((member, i) =>
      argRows(
        member,
        indent + INDENT,
        columns,
        i < members.length - 1 ? "," : "",
        noChip,
      ).map((row) => ({
        ...row,
        tokens: row.tokens.map(({ kind, text }) => ({ kind, text })),
      })),
    ),
    { indent, tokens: [{ kind: "sep", text: close }, ...tail] },
  ];
}
