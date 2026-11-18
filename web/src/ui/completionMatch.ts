import type { EditorState } from "@tiptap/pm/state";

export interface CompletionMatch {
  from: number;
  to: number;
  prefix: string;
  kind: "paths" | "agents";
}

/** Literal Markdown remains text in the editor. Suppress discovery inside
 * fenced and inline code rather than treating code spelling as an invitation
 * to browse (docs/web-client.md). */
function inCode(text: string): boolean {
  let fence: string | null = null;
  let ticks = 0;
  for (const line of text.split("\n")) {
    const marker = /^ {0,3}(`{3,}|~{3,})/.exec(line)?.[1];
    if (marker && ticks === 0) {
      if (fence === null) fence = marker;
      else if (marker[0] === fence[0] && marker.length >= fence.length)
        fence = null;
      continue;
    }
    if (fence !== null) continue;
    for (const run of line.matchAll(/`+/g)) {
      if (ticks === 0) ticks = run[0].length;
      else if (ticks === run[0].length) ticks = 0;
    }
  }
  return fence !== null || ticks !== 0;
}

const leafText = (node: import("@tiptap/pm/model").Node) =>
  node.type.name === "hardBreak" ? "\n" : "\ufffc";

export function completionMatch(state: EditorState): CompletionMatch | null {
  const { empty, $from, from } = state.selection;
  if (!empty || !$from.parent.isTextblock) return null;
  const before = $from.parent.textBetween(
    0,
    $from.parentOffset,
    "\n",
    leafText,
  );
  const after = $from.parent.textBetween(
    $from.parentOffset,
    $from.parent.content.size,
    "\n",
    leafText,
  );
  // Don't replace the first half of a token when the caret moves within it.
  if (after && !/^[\s)\]}>,;!?"']/.test(after)) return null;
  const found =
    /(?:^|[\s([{"'])((?:\.\/|\.\.\/|~\/|\/)[^\s\ufffc`<>"'\])}]*|@[a-zA-Z0-9_-]*)$/.exec(
      before,
    );
  if (!found) return null;
  const prefix = found[1];
  const context = state.doc.textBetween(0, from, "\n", leafText);
  if (inCode(context)) return null;
  return {
    from: from - prefix.length,
    to: from,
    prefix,
    kind: prefix.startsWith("@") ? "agents" : "paths",
  };
}

export const matchKey = (match: CompletionMatch) =>
  `${match.from}:${match.to}:${match.kind}:${match.prefix}`;
