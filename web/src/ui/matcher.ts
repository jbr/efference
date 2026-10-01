// The picker's matching rule (docs/tui.md), as pure functions over a flat node
// list — the TypeScript reading of `src/tui/session_form/matcher.rs`. The two
// run the same fixture (`src/tui/session_form/match_cases.json`), which is the
// contract: where this port and the Rust disagree, the fixture decides and one
// of them is wrong.
//
// A node is a connection, a group inside a connection (OpenRouter's authors),
// or a model. The picker's chips scope the list to a subtree. A node's *slug*
// is relative to the chips: the whole `connection/model` at the root, `model`
// under a connection chip, and the model with its `author/` prefix dropped (a
// leading `~` alias marker kept) under a group chip. A node's *own text* is
// what it is called inside its parent: a connection's name, a group's name, a
// model's id without its author.
//
// The rule, per whitespace-separated token of the lowercased query (every token
// must match). A token without `/` is matched against the node's own text, so a
// row is not found through the names of its ancestors; a token with `/` is
// matched against the slug. A token's tier is the best of:
//
// 0. the token equals the final `/`-segment of the text;
// 1. the token is a prefix of the text read from the start or from just after
//    any of `/ - . : ~`;
// 2. the token is a substring of the text or of the node's `name`;
// 3. the token is a subsequence of the text.
//
// A row's tier is its worst token's. Rows sort by tier, then interior rows
// before leaves, then (when recents count) recent rank, then list order.

/** What a node is. */
export type Kind = "connection" | "group" | "model";

/** One row of the catalog tree, flat — the shape `match_cases.json` stores. */
export interface Node {
  kind: Kind;
  connection: string;
  group?: string;
  /** The provider-native id; absent for interior nodes. */
  model?: string;
  name?: string;
  /** Rank of the matching recent pick, 1 first. */
  recent?: number;
}

/** One segment of the picker's path. Which depth it is at says what it names:
 * the first is a connection, the second a group. */
export type Chip = string;

const SEPARATORS = /[/\-.:~]/;

/** A grouped model id without its `author/` prefix; a leading `~` alias marker
 * stays, since `~claude-latest` and `claude-latest` are different ids. */
export function stripAuthor(model: string): string {
  const marker = model.startsWith("~") ? "~" : "";
  const cut = model.indexOf("/");
  return cut === -1 ? model : `${marker}${model.slice(cut + 1)}`;
}

/** The node's full slug, the id the fixture names it by: `connection`,
 * `connection/group`, `connection/model`. */
export function id(node: Node): string {
  if (node.kind === "model") return `${node.connection}/${node.model ?? ""}`;
  if (node.kind === "group" && node.group !== undefined) {
    return `${node.connection}/${node.group}`;
  }
  return node.connection;
}

/** The slug as the picker shows and matches it under `chips`. */
export function relative(node: Node, chips: Chip[]): string {
  if (node.kind === "connection") return node.connection;
  if (chips.length === 0) return id(node);
  if (node.kind === "group") return node.group ?? "";
  if (chips.length === 1) return node.model ?? "";
  return stripAuthor(node.model ?? "");
}

/** What the node is called inside its parent. */
export function own(node: Node): string {
  if (node.kind === "connection") return node.connection;
  if (node.kind === "group") return node.group ?? "";
  return node.group !== undefined
    ? stripAuthor(node.model ?? "")
    : (node.model ?? "");
}

/** Whether the node sits inside the subtree the chips name. */
export function inScope(node: Node, chips: Chip[]): boolean {
  if (chips.length === 0) return true;
  if (chips.length === 1) {
    return node.connection === chips[0] && node.kind !== "connection";
  }
  if (chips.length === 2) {
    return (
      node.kind === "model" &&
      node.connection === chips[0] &&
      node.group === chips[1]
    );
  }
  return false;
}

export const isInterior = (node: Node): boolean => node.kind !== "model";

/** Whether `token` appears in `slug` in order, not necessarily adjacently — the
 * last tier, so a row that is only a subsequence still surfaces. */
function subsequence(token: string, slug: string): boolean {
  let at = 0;
  for (const character of slug) {
    if (character === token[at]) at += 1;
    if (at === token.length) return true;
  }
  return token.length === 0;
}

/** Whether `token` is a prefix of `slug` read from its start or from just after
 * one of `/ - . : ~`, which is what "at a segment start" means. */
function prefixAtBoundary(token: string, slug: string): boolean {
  for (let start = 0; start <= slug.length;) {
    if (slug.startsWith(token, start)) return true;
    const separator = slug.slice(start).search(SEPARATORS);
    if (separator === -1) return false;
    start += separator + 1;
  }
  return false;
}

/** One token's tier against a lowercased slug and name; `null` when it misses. */
function tier(
  token: string,
  slug: string,
  name: string | undefined,
): number | null {
  if (slug.split("/").pop() === token) return 0;
  if (prefixAtBoundary(token, slug)) return 1;
  if (slug.includes(token) || (name !== undefined && name.includes(token))) {
    return 2;
  }
  if (subsequence(token, slug)) return 3;
  return null;
}

/** The indexes of the nodes in scope that match `query`, best first.
 * `recents` is true for the main role, the only role whose picks are recorded. */
export function rank(
  nodes: Node[],
  chips: Chip[],
  query: string,
  recents: boolean,
): number[] {
  const tokens = query.toLowerCase().split(/\s+/).filter(Boolean);
  const hits: { key: [number, boolean, number, number]; index: number }[] = [];

  nodes.forEach((node, index) => {
    if (!inScope(node, chips)) return;
    const slug = relative(node, chips).toLowerCase();
    const text = own(node).toLowerCase();
    const name = node.name?.toLowerCase();

    // A row's tier is its worst token's, and one token that misses drops it:
    // every token must match.
    let worst = 0;
    for (const token of tokens) {
      const scored = tier(token, token.includes("/") ? slug : text, name);
      if (scored === null) return;
      worst = Math.max(worst, scored);
    }

    hits.push({
      key: [
        worst,
        // interior rows ahead of leaves within a tier
        !isInterior(node),
        recents ? (node.recent ?? Number.MAX_SAFE_INTEGER) : 0,
        index,
      ],
      index,
    });
  });

  hits.sort((a, b) => {
    for (let i = 0; i < a.key.length; i += 1) {
      if (a.key[i] < b.key[i]) return -1;
      if (a.key[i] > b.key[i]) return 1;
    }
    return 0;
  });
  return hits.map((hit) => hit.index);
}
