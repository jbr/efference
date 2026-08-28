// Where an edit landed, found in the file as it is now
// (docs/web-client.md "The file viewer").
//
// The viewer's other source of "where to look" is the since-you-last-looked
// diff, which is empty on a file's first open — so clicking an edit's `open ⤢`
// used to land at the top of the file with nothing highlighted, and follow
// switched files without landing on the edit.
//
// This is the second source: the edit itself. An edit tool's result carries a
// `===DIFF===` section (render/parse.ts `diffRows`, the same payload the
// transcript colours), and the lines it inserted are the anchor.
//
// **Content, not line numbers.** The payload carries no hunk headers — no
// `@@ -a,b +c,d @@`, verified across the whole local corpus — so there are no
// post-edit line numbers to trust even in the live case, and the anchor has to
// be the inserted text located in the current file. That turns out to be the
// resolution the two cases wanted anyway:
//
// - **Following live** — the edit just landed, the text is on disk verbatim,
//   the search finds it exactly once, and the viewer jumps and highlights.
// - **A historical edit** — later edits may have moved, reflowed or removed
//   those lines. A unique match is still a match, and still exact. Otherwise
//   the answer is honest approximation, never a confident highlight in the
//   wrong place: an ambiguous or context-only match jumps and says the position
//   is approximate; nothing found at all says so and does not jump.
//
// Whitespace: a match is tried verbatim first and then with each line trimmed,
// because the structured editors reformat the file around an edit and a
// re-indented block is the same block.

import { diffRows } from "../render/parse.ts";

/** The lines an edit inserted, with the context it sat between — enough to
 * find the edit again in a file that has moved on. */
export interface DiffAnchor {
  /** Inserted lines of the first changed region, `+` stripped. */
  added: string[];
  /** Context lines immediately before that region. */
  before: string[];
  /** Context lines immediately after it. */
  after: string[];
}

/** Where the viewer should look, and how sure it is.
 *
 * - `exact` — the inserted lines were found; `changed` is theirs to highlight.
 * - `approximate` — the position is a guess (ambiguous match, or context only);
 *   the viewer jumps and says so rather than highlighting.
 * - `unlocated` — the edit's lines are not in this file any more.
 */
export type AnchorMatch =
  | { kind: "exact"; line: number; changed: [number, number][] }
  | { kind: "approximate"; line: number }
  | { kind: "unlocated" };

/** The first changed region of an edit's `===DIFF===` result, as an anchor.
 * `null` when the result carries no diff, or a diff that inserted and removed
 * nothing. */
export function anchorFromResult(content: string): DiffAnchor | null {
  const rows = diffRows(content);
  if (rows === null) return null;
  const lines = rows.map(([, line]) => line);

  const first = lines.findIndex(
    (line) => line.kind === "added" || line.kind === "removed",
  );
  if (first === -1) return null;

  const before: string[] = [];
  for (let i = first - 1; i >= 0 && lines[i].kind === "context"; i--) {
    before.unshift(lines[i].text.slice(1));
  }
  const added: string[] = [];
  let i = first;
  for (; i < lines.length && lines[i].kind !== "context"; i++) {
    if (lines[i].kind === "added") added.push(lines[i].text.slice(1));
  }
  const after: string[] = [];
  for (; i < lines.length && lines[i].kind === "context"; i++) {
    after.push(lines[i].text.slice(1));
  }
  return { added, before, after };
}

/** Find `anchor` in `text`. */
export function locateAnchor(anchor: DiffAnchor, text: string): AnchorMatch {
  const file = text.split("\n");

  if (anchor.added.length > 0) {
    const hits = occurrences(file, anchor.added);
    if (hits.length === 1) return exact(hits[0], anchor.added.length);
    if (hits.length > 1) {
      // Several copies of the same insertion: the context around it is what
      // tells them apart, and only a unique answer earns a highlight.
      const withContext = hits.filter((at) => contextFits(file, at, anchor));
      if (withContext.length === 1)
        return exact(withContext[0], anchor.added.length);
      return { kind: "approximate", line: hits[0] + 1 };
    }
  }

  // The insertion is gone, was never there (a pure deletion), or was
  // reformatted past a trimmed comparison. The context it sat between still
  // says roughly where to look.
  const beforeAt =
    anchor.before.length > 0 ? occurrences(file, anchor.before) : [];
  if (beforeAt.length === 1) {
    return {
      kind: "approximate",
      line: beforeAt[0] + anchor.before.length + 1,
    };
  }
  const afterAt =
    anchor.after.length > 0 ? occurrences(file, anchor.after) : [];
  if (afterAt.length === 1)
    return { kind: "approximate", line: afterAt[0] + 1 };
  return { kind: "unlocated" };
}

const exact = (at: number, length: number): AnchorMatch => ({
  kind: "exact",
  line: at + 1,
  changed: [[at + 1, at + length]],
});

/** Whether the context the edit sat between is still around a hit — the
 * tiebreak when the same lines appear more than once. Partial credit: as much
 * of each side as the file has. */
function contextFits(file: string[], at: number, anchor: DiffAnchor): boolean {
  const before = anchor.before.at(-1);
  const after = anchor.after.at(0);
  const beforeOk = before === undefined || same(file[at - 1], before);
  const afterOk =
    after === undefined || same(file[at + anchor.added.length], after);
  return beforeOk && afterOk;
}

const same = (a: string | undefined, b: string): boolean =>
  a !== undefined && (a === b || a.trim() === b.trim());

/** Every 0-based index in `file` where `needle` appears as a run of lines.
 * Verbatim first; a trimmed comparison only if that found nothing, so a
 * reformatted-but-unmoved block still resolves and an exact match is never
 * traded for a loose one. */
function occurrences(file: string[], needle: string[]): number[] {
  if (needle.length === 0 || needle.length > file.length) return [];
  const scan = (equal: (a: string, b: string) => boolean): number[] => {
    const hits: number[] = [];
    for (let at = 0; at + needle.length <= file.length; at++) {
      let ok = true;
      for (let i = 0; i < needle.length; i++) {
        if (!equal(file[at + i], needle[i])) {
          ok = false;
          break;
        }
      }
      if (ok) hits.push(at);
    }
    return hits;
  };
  const verbatim = scan((a, b) => a === b);
  if (verbatim.length > 0) return verbatim;
  // An all-blank needle would match everywhere once trimmed; it says nothing
  // about position, so it says nothing at all.
  if (needle.every((line) => line.trim() === "")) return [];
  return scan((a, b) => a.trim() === b.trim());
}
