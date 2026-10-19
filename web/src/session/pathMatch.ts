// Matching a typed filter against project paths — the file sidebar's path
// filter (docs/web-client.md "Navigating the working directory").
//
// A filter, not a fuzzy palette: each space-separated word of the query must
// appear in the path as written, contiguously, and every word must appear. A
// list in which `rough` brings up `btn-group-right` has stopped saying why each
// row is there. Case-insensitive unless the query has an uppercase letter in
// it, which is the smart-case rule from grep — a reader who typed a capital
// meant it.
//
// Ranking is deliberately plain. Each word scores its best occurrence: inside
// the **basename** (what you are usually naming), at the **start of a path
// segment**, and most of all at the start of the basename. Ties break on the
// shorter path, so the file beats the file in the vendored copy of it.
//
// Matching only path *names*. Content search is a server-side scan with its own
// cost and ranking questions and stays unbuilt.

export interface PathHit {
  path: string;
  score: number;
  /** The matched spans as `[start, end)` character offsets, sorted and
   * merged, for highlighting. */
  matched: [number, number][];
}

/** Score `path` against `query`, or `null` if some word of the query does not
 * occur in it. An empty query matches everything at score 0. */
export function matchPath(query: string, path: string): PathHit | null {
  const smartCase = query !== query.toLowerCase();
  const haystack = smartCase ? path : path.toLowerCase();
  const words = (smartCase ? query : query.toLowerCase())
    .split(/\s+/)
    .filter((word) => word !== "");
  if (words.length === 0) return { path, score: 0, matched: [] };

  const basenameAt = path.lastIndexOf("/") + 1;
  const spans: [number, number][] = [];
  let score = 0;
  for (const word of words) {
    let best: { at: number; score: number } | null = null;
    for (
      let at = haystack.indexOf(word);
      at !== -1;
      at = haystack.indexOf(word, at + 1)
    ) {
      let points = 0;
      if (at >= basenameAt) points += 5;
      if (at === 0 || haystack[at - 1] === "/") points += 6;
      if (at === basenameAt) points += 4;
      if (best === null || points > best.score) best = { at, score: points };
    }
    if (best === null) return null;
    score += best.score;
    spans.push([best.at, best.at + word.length]);
  }
  // Shorter paths win ties: the direct hit beats the vendored copy of it.
  score -= Math.floor(path.length / 12);
  return { path, score, matched: merge(spans) };
}

/** Overlapping or touching spans as one, in order. */
function merge(spans: [number, number][]): [number, number][] {
  const sorted = [...spans].sort((a, b) => a[0] - b[0]);
  const out: [number, number][] = [];
  for (const [start, end] of sorted) {
    const last = out[out.length - 1];
    if (last !== undefined && start <= last[1])
      last[1] = Math.max(last[1], end);
    else out.push([start, end]);
  }
  return out;
}

/** The best `limit` matches, best first. */
export function matchPaths(
  query: string,
  paths: readonly string[],
  limit = 50,
): PathHit[] {
  const hits: PathHit[] = [];
  for (const path of paths) {
    const hit = matchPath(query, path);
    if (hit !== null) hits.push(hit);
  }
  hits.sort((a, b) => b.score - a.score || a.path.localeCompare(b.path));
  return hits.slice(0, limit);
}
