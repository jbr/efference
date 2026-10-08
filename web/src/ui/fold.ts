// What the web client offers for a fold (docs/web-client.md): the one rule both
// the composer's cold notice and the drawer's standing button read. The daemon
// owns the real conditions (`409 not_foldable`); this only decides whether to
// offer the action and what to tell the operator before it runs, from the three
// topics the answer depends on.

import type { Cold, Fold } from "../api/types.ts";

export interface FoldOffer {
  /** `writing`: a fold is in flight. `blocked`: not now, and `reason` says why. */
  kind: "ready" | "writing" | "blocked";
  reason?: string;
  /** Whether the fold gives up a live cache, so the operator confirms first. */
  warm: boolean;
}

export function foldOffer(
  cold: Cold | null,
  fold: Fold | null,
  running: boolean,
  gatePending: boolean,
  asking: boolean,
): FoldOffer {
  const warm = cold === null;
  if (fold?.state === "writing") return { kind: "writing", warm };
  if (gatePending) {
    return {
      kind: "blocked",
      reason: "an approval gate is pending — resolve it first",
      warm,
    };
  }
  if (asking) {
    return {
      kind: "blocked",
      reason: "a question is pending — answer it first",
      warm,
    };
  }
  // A loop parked at its cold check reads as running, and may fold; a warm one
  // that is running is mid-request.
  if (running && warm) {
    return {
      kind: "blocked",
      reason: "the loop is running — summarize when it stops",
      warm,
    };
  }
  return { kind: "ready", warm };
}

/** The sentence a confirm shows before a fold runs. */
export function foldWarning(cold: Cold | null): string {
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
