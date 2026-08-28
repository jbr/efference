// The composer status bar's turn-state readout (docs/web-client.md). Three
// conditions, from two level-triggered topics: the loop is running, it is idle
// after finishing, or it is idle after giving up (docs/transport.md). The third
// is the one `running` alone cannot say — a stalled session and a finished one
// are the same `false` — so the `stall` topic decides it.

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
 * The readout for the current pair of topic values. A pure function: no clock
 * is read, so the `at` the stall carries is not rendered as an elapsed time.
 *
 * A stall while `running` is true cannot happen — the daemon clears the topic
 * as a trajectory starts — but if a client sees that pair across a reconnect,
 * running wins: it describes what the daemon is doing now.
 */
export function turnState(running: boolean, stall: Stall | null): TurnState {
  if (running) return { text: "running…", kind: "running" };
  if (stall !== null) {
    return { text: stall.message, title: stall.detail, kind: "stalled" };
  }
  return { text: "your turn", kind: "idle" };
}
