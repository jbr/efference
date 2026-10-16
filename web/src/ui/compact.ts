// What the web client offers for a compaction (docs/web-client.md): the one rule both
// the composer's cold notice and the drawer's standing button read. The compact route's
// own answer is the `verbs` topic's `compact` entry (docs/control-api.md), so the offer
// is that answer, plus a compaction already in flight from `current_operations`.

import type { Cold, CurrentOperations, Verbs } from "../api/types.ts";

export interface CompactOffer {
  /** `writing`: a compaction is in flight. `blocked`: not now, and `reason` says why. */
  kind: "ready" | "writing" | "blocked";
  reason?: string;
  /** Whether the compaction gives up a live cache, so the operator confirms first. */
  warm: boolean;
}

export function compactOffer(
  verbs: Verbs | null,
  operations: CurrentOperations | null,
  cold: Cold | null,
): CompactOffer {
  const open = operations?.compaction ?? null;
  if (open !== null) return { kind: "writing", warm: open.warm };
  const verb = verbs?.compact;
  // Before the topic lands, the route answers for itself.
  if (verb === undefined) return { kind: "ready", warm: cold === null };
  if (!verb.accepted) {
    return { kind: "blocked", reason: verb.message, warm: cold === null };
  }
  return { kind: "ready", warm: verb.warm };
}

/** The sentence a confirm shows before a compaction runs. */
export function compactionWarning(cold: Cold | null): string {
  const read =
    cold === null
      ? "The prompt cache is live, and the next request after the summary reads the new, shorter context uncached."
      : "The cache has expired, so this replaces paying for the full uncached read.";
  return (
    "The session's model writes a summary of the conversation so far, and " +
    "about the last 15k tokens of work stay verbatim after it. " +
    read +
    " The full log is kept, but there is no undo yet."
  );
}

/** Tokens in the compact form the cold notice uses: `120k`. */
export function kilotokens(tokens: number): string {
  return `${Math.round(tokens / 1000)}k`;
}
