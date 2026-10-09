// The composer status bar's turn-state readout (docs/web-client.md). Four
// conditions, from three level-triggered topics: the loop is running, it is
// waiting to retry the upstream, it is idle after finishing, or it has stopped
// short (docs/transport.md). A stall is the one `running` alone cannot say — a
// stalled session and a finished one are the same `false`, and a triage gate
// held on the upstream is stalled while still `true` — so the `stall` topic
// decides it. A loop parked at the cold check (the `park` topic) is `running` too, and
// reads as its own condition: it waits on the operator. A fold in flight (the `fold` topic) reads as running: the loop is
// idle, but the session is busy writing its summary. Both the retry wait and the stall name the recovery: an empty
// send, which the daemon takes as "retry now".

import type { Park, Stall } from "../api/types.ts";
import { kilotokens } from "./fold.ts";

export interface TurnState {
  /** What the readout says. */
  text: string;
  /** Hover text: the upstream's own words for a stall, nothing otherwise. */
  title?: string;
  /** Which condition, for the dot and the styling. */
  kind: "running" | "idle" | "stalled" | "parked";
}

/**
 * The readout for the current topic values. A pure function: no clock is read,
 * so neither the stall's `at` nor the retry's `next_attempt_at` is rendered as
 * a time.
 */
export function turnState(
  running: boolean,
  stall: Stall | null,
  retrying = false,
  folding = false,
  park: Park | null = null,
): TurnState {
  if (stall !== null) {
    return {
      text: `${stall.message} · send to retry`,
      title: stall.detail,
      kind: "stalled",
    };
  }
  // The loop is stopped at its cold check, not slow: `running` stays true while
  // it waits. The composer's cold notice carries the choice (send or summarize).
  if (running && !folding && park !== null) {
    if (park.reason === "folded") {
      return {
        text: "paused: summarized · send to continue",
        title:
          "the history was summarized while the loop was stopped; a send or an empty send resumes it, with no consent needed",
        kind: "parked",
      };
    }
    const read =
      park.context_tokens === null
        ? "the whole context"
        : `${kilotokens(park.context_tokens)} tokens`;
    return {
      text: `paused: cache expired · send to continue (reads ${read} uncached)`,
      title:
        "the loop stopped before a request that would read the expired cache; send to consent, or summarize instead",
      kind: "parked",
    };
  }
  if (folding) {
    return {
      text: "summarizing the history…",
      title: "the session's model is writing the summary",
      kind: "running",
    };
  }
  if (running && retrying) {
    return {
      text: "retrying the upstream · send to retry now",
      kind: "running",
    };
  }
  if (running) return { text: "running…", kind: "running" };
  return { text: "your turn", kind: "idle" };
}
