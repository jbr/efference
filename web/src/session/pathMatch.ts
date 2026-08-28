// Matching a typed fragment against project paths — the file finder's ranking
// (docs/web-client.md "Navigating the working directory").
//
// Subsequence matching, not substring: `wcl` should find
// `web/src/ui/FileView.tsx`'s neighbours the way every palette does, and typing
// `backlog/INDEX` should beat three tree expansions. Case-insensitive unless
// the query has an uppercase letter in it, which is the smart-case rule from
// grep — a reader who typed a capital meant it.
//
// Ranking is deliberately plain. Three things earn points: a match inside the
// **basename** (what you are usually naming), **consecutive** matched
// characters (a real fragment beats scattered letters), and a match at a
// **segment boundary** (the start of a path component). Ties break on the
// shorter path, so the file beats the file in the vendored copy of it.
//
// Matching only path *names*. Content search is a server-side scan with its own
// cost and ranking questions and stays unbuilt.

export interface PathHit {
  path: string;
  score: number;
  /** 0-based indices of the matched characters, for highlighting. */
  matched: number[];
}

/** Score `path` against `query`, or `null` if the query is not a subsequence
 * of it. An empty query matches everything at score 0. */
export function matchPath(query: string, path: string): PathHit | null {
  if (query === "") return { path, score: 0, matched: [] };
  const smartCase = query !== query.toLowerCase();
  const haystack = smartCase ? path : path.toLowerCase();
  const needle = smartCase ? query : query.toLowerCase();

  const basenameAt = path.lastIndexOf("/") + 1;
  const matched: number[] = [];
  let score = 0;
  let at = 0;
  let previous = -2;
  for (const ch of needle) {
    if (ch === " ") continue;
    const found = haystack.indexOf(ch, at);
    if (found === -1) return null;
    matched.push(found);
    if (found === previous + 1) score += 8;
    if (found >= basenameAt) score += 5;
    if (found === 0 || haystack[found - 1] === "/") score += 6;
    if (found === basenameAt) score += 4;
    previous = found;
    at = found + 1;
  }
  // Shorter paths win ties: the direct hit beats the vendored copy of it.
  score -= Math.floor(path.length / 12);
  return { path, score, matched };
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
