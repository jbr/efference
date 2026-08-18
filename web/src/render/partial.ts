// What has arrived of a tool call's JSON input, mid-stream — the TypeScript
// mirror of `src/tui/projection/partial.rs` (docs/web-client.md).
//
// A tool call's arguments arrive as fragments of a JSON document, so at any
// moment the accumulated buffer is a *prefix* of one —
// `{"path":"src/api.rs","contents":"use st`. The rule that keeps rendering it
// honest is **positional**: within an object, only the **last** key can be
// incomplete, because the bytes for the earlier ones already arrived and
// closed. So the settled pairs may be rendered as identity — this call *is* a
// write to `src/api.rs` — while the last pair is still arriving and may only
// be rendered as content. Rendering a half-arrived `"src/api.r` as a path
// would be a well-formed, plausible, false claim, which is worse than showing
// raw bytes.
//
// The split is read off the parser rather than guessed: `partial-json`'s
// `Allow.COLLECTION` drops an incomplete trailing value along with its key,
// and `Allow.COLLECTION | Allow.STR` keeps it — so the one key the second
// parse has and the first lacks is exactly the value still in flight. (The
// same differencing the TUI reads off jiter's `PartialMode::On` vs
// `PartialMode::TrailingStrings`.)

import { Allow, parse } from "partial-json";
import { langForPath } from "./parse.ts";

/** A tool call's input as far as it has arrived. */
export interface PartialInput {
  /** Complete key/value pairs in arrival order — JS object insertion order,
   * which holds for the non-numeric keys tool inputs use. Every value here is
   * final, with one nested exception the TUI shares: a string still arriving
   * *inside* a settled container stays where it is, since only the top level
   * is differenced. Tool inputs are flat, so the case does not arise. */
  settled: Record<string, unknown>;
  /** The pair still arriving: its key, and how much of its value has decoded.
   * Always a string — the only thing the loose parse keeps that the strict
   * one drops. `null` once the document closes, and for a buffer that ends on
   * a pair boundary. */
  arriving: { key: string; text: string } | null;
}

/**
 * Read `buffer` — a prefix of a JSON object — into settled and arriving pairs.
 *
 * `null` when the buffer holds no object at all (empty, or a top-level value
 * that isn't an object): the caller falls back to showing the raw characters,
 * which claim nothing. A parse whose settled *and* arriving are both empty
 * (`{`, `{"pa`) means the same fallback — the TUI draws that line in the
 * caller, and so does `ToolBlocks.tsx`.
 */
export function partialInput(buffer: string): PartialInput | null {
  // The trailing-strings parse is the fuller of the two; if even it cannot
  // make an object out of the prefix, there is nothing structural to show.
  const loose = parseObject(buffer, Allow.COLLECTION | Allow.STR);
  if (loose === null) return null;
  // Whatever the strict parse also has is closed. It can come up empty where
  // the loose one succeeds (a buffer that is *only* an incomplete pair),
  // which just means nothing has settled yet.
  const strictCount = countKeys(parseObject(buffer, Allow.COLLECTION));

  const settled = { ...loose };
  const keys = Object.keys(loose);
  // Only the final pair can be incomplete, so anything the strict parse
  // dropped is that pair and nothing else.
  if (keys.length <= strictCount) return { settled, arriving: null };
  const key = keys[keys.length - 1];
  const text = settled[key];
  delete settled[key];
  return {
    settled,
    arriving: typeof text === "string" ? { key, text } : null,
  };
}

/** Whether anything structural has arrived — the raw-characters fallback
 * applies below this line. */
export function hasStructure(input: PartialInput): boolean {
  return Object.keys(input.settled).length > 0 || input.arriving !== null;
}

/** The language to highlight an in-flight value in: taken from whichever
 * settled argument names a file. Only the settled ones are consulted — a path
 * that is itself still arriving names nothing yet. */
export function streamingLang(settled: Record<string, unknown>): string | null {
  for (const key of ["path", "file_path"]) {
    const value = settled[key];
    if (typeof value === "string") return langForPath(value);
  }
  return null;
}

function parseObject(
  buffer: string,
  allow: number,
): Record<string, unknown> | null {
  let value: unknown;
  try {
    value = parse(buffer, allow);
  } catch {
    // Nothing parseable yet (an empty buffer, or a lone `-`): not an object.
    return null;
  }
  if (typeof value !== "object" || value === null || Array.isArray(value)) {
    // A tool call's input is always an object; anything else is not a case
    // to invent a rendering for.
    return null;
  }
  return value as Record<string, unknown>;
}

const countKeys = (obj: Record<string, unknown> | null): number =>
  obj === null ? 0 : Object.keys(obj).length;
