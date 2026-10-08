// The composer status bar's turn-state readout (docs/web-client.md). Four
// conditions, from three level-triggered topics: the loop is running, it is
// waiting to retry the upstream, it is idle after finishing, or it has stopped
// short (docs/transport.md). A stall is the one `running` alone cannot say — a
// stalled session and a finished one are the same `false`, and a triage gate
// held on the upstream is stalled while still `true` — so the `stall` topic
// decides it. A fold in flight (the `fold` topic) reads as running: the loop is
// idle, but the session is busy writing its summary. Both the retry wait and the stall name the recovery: an empty
// send, which the daemon takes as "retry now".

import type { Stall } from "../api/types.ts";

export interface TurnState {
  /** What the readout says. */
  text: string;
  /** Hover text: the upstream's own words for a stall, nothing otherwise. */
  title?: string;
  /** Which condition, for the dot and the styling. */
  kind: "running" | "idle" | "stalled";
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
): TurnState {
  if (stall !== null) {
    return {
      text: `${stall.message} · send to retry`,
      title: stall.detail,
      kind: "stalled",
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
