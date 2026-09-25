// The chip table: a lookup keyed by `(tool, argument)` deciding which arguments
// of a signature render as more than their text (docs/web-client.md "An
// argument can carry a chip"). What a chip *is* is decided here, from the
// argument alone; resolving one — a fetch, a click handler — is `chips.tsx`.
//
// The rule the table is held to: **a chip adds an affordance or a fact the
// argument does not carry** — a link, a resolved handle. Never a re-wording, so
// `search`, `read`, and `list` have no entries: their arguments already say
// everything, and `Searched src for "Listening"` is strictly more words than
// `search(pattern: "Listening", path: "src")`.

import { viewablePath } from "../session/touchedFiles.ts";

/** What an argument's chip is. */
export type ChipSpec =
  { kind: "path"; path: string } | { kind: "process"; id: number };

type ChipOf = (value: unknown) => ChipSpec | null;

/** A path the file viewer can serve — within-cwd only
 * (session/touchedFiles.ts), so an absolute path yields no chip and the call's
 * own diff stays its only surface. */
const pathChip: ChipOf = (value) => {
  if (typeof value !== "string") return null;
  const path = viewablePath(value);
  return path === null ? null : { kind: "path", path };
};

/** A supervisor handle, spelled `p#<n>` as the model writes it (mirrors
 * `supervisor::parse_handle`). A bare ordinal is accepted too — the tools take
 * the string form, but nothing here depends on that. */
const processChip: ChipOf = (value) => {
  const id =
    typeof value === "number"
      ? value
      : typeof value === "string" && value.startsWith("p#")
        ? Number(value.slice(2))
        : NaN;
  return Number.isInteger(id) && id >= 0 ? { kind: "process", id } : null;
};

/** The whole table. Two renderers over seven entries — a lookup, not a
 * framework: a tool earns a line here only by having an argument that cannot
 * speak for itself. */
const CHIPS: Record<string, Record<string, ChipOf>> = {
  write: { path: pathChip },
  edit: { file_path: pathChip },
  undo_edit: { file_path: pathChip },
  retarget_edit: { file_path: pathChip },
  kill_process: { handle: processChip },
  read_process: { handle: processChip },
  watch_process: { handle: processChip },
};

/** The chip one argument of one call carries, or null for the usual case of an
 * argument that says everything already. `tool` is the wire name, so a mounted
 * server's same-named tool gets no built-in's chip. */
export function argChip(
  tool: string,
  key: string,
  value: unknown,
): ChipSpec | null {
  const of = CHIPS[tool]?.[key];
  return of === undefined ? null : of(value);
}

/** How much of a spawning command a chip shows: enough to recognize which
 * process it was, never enough to become the line. */
const COMMAND_BUDGET = 32;

/** The columns a chip adds beyond the argument's own text, for the signature's
 * width arithmetic (signatureLayout.ts): the path chip's ` ⤢`, or the process
 * chip's `⟨p#… · command⟩` at its longest. An upper bound, so a chipped
 * argument breaks early rather than overrunning. */
export function chipExtra(tool: string, key: string, value: unknown): number {
  const chip = argChip(tool, key, value);
  if (chip === null) return 0;
  return chip.kind === "path" ? 2 : COMMAND_BUDGET + 8;
}

export function chipCommand(command: string): string {
  const line = command.split("\n")[0].trim();
  return line.length > COMMAND_BUDGET
    ? `${line.slice(0, COMMAND_BUDGET).trimEnd()} …`
    : line;
}
