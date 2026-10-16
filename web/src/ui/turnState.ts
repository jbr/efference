// The composer status bar's turn-state readout (docs/web-client.md), read off
// the `loop_phase` topic (docs/control-api.md) with the topics that say more
// about a phase: `stall` (a trajectory that gave up, idle or with its triage
// held), `cold` (what a cold park is waiting to read), `upstream_retry` (the
// pause between attempts), and `current_operations` (what a running loop is
// waiting on, and a compaction in flight). A stall, a cold park, a gate or ask,
// and a block wait on the operator, and each names the gesture that moves it on.

import type {
  Cold,
  CurrentOperations,
  LoopPhase,
  Stall,
} from "../api/types.ts";
import { kilotokens } from "./compact.ts";
import { operationText, operationTitle } from "./operations.ts";

/** The `current_operations` topic with what naming it needs: the session's
 * model, and the client's clock (the topic carries absolute instants). */
export interface OperationsView {
  operations: CurrentOperations | null;
  model: string;
  now: number;
}

export interface TurnState {
  /** What the readout says. */
  text: string;
  /** Hover text: the upstream's own words for a stall, nothing otherwise. */
  title?: string;
  /** Which condition, for the dot and the styling. */
  kind: "running" | "idle" | "stalled" | "parked" | "waiting";
}

/** What the readout reads besides the phase. */
export interface TurnTopics {
  stall?: Stall | null;
  cold?: Cold | null;
  /** A retry of the upstream is waiting (the `upstream_retry` topic). */
  retrying?: boolean;
  view?: OperationsView | null;
}

/**
 * The readout for the current topic values. A pure function: no clock is read,
 * so neither the stall's `at` nor the retry's `next_attempt_at` is rendered as
 * a time; the operation's elapsed time and a pace delay's remainder are the
 * places a clock matters, and it arrives as `view.now`.
 */
export function turnState(
  phase: LoopPhase | null,
  { stall = null, cold = null, retrying = false, view = null }: TurnTopics = {},
): TurnState {
  const compacting = (view?.operations?.compaction ?? null) !== null;
  const current = phase ?? { phase: "idle" };
  if (stall !== null) {
    return {
      text: `${stall.message} · send to retry`,
      title: stall.detail,
      kind: "stalled",
    };
  }
  // The loop is stopped at its cold check, not slow. The composer's cold notice
  // carries the choice (send or summarize).
  if (current.phase === "cold_parked" && !compacting) {
    if (cold === null) {
      return {
        text: "paused: summarized · send to continue",
        title:
          "the history was summarized while the loop was stopped; a send or an empty send resumes it",
        kind: "parked",
      };
    }
    const read =
      cold.context_tokens === null
        ? "the whole context"
        : `${kilotokens(cold.context_tokens)} tokens`;
    return {
      text: `paused: cache expired · send to continue (reads ${read} uncached)`,
      title:
        "the loop stopped before a request that would read the expired cache; send to continue, or summarize instead",
      kind: "parked",
    };
  }
  // An open gate or `ask` is what the operator must act on, ahead of any
  // request or tool the loop has open beside it.
  if (current.phase === "gated" || current.phase === "asking") {
    return {
      text:
        current.phase === "gated"
          ? "waiting for your approval"
          : "waiting for your answer",
      kind: "waiting",
    };
  }
  if (current.phase === "blocked") {
    return current.reason === "pace_held"
      ? {
          text: "held · resume or send to continue",
          title: "the pace is held; nothing runs until it is resumed",
          kind: "parked",
        }
      : {
          text: "stopped: the config file needs repair",
          title:
            "the config file does not parse and could not be restored; the loop waits until it is fixed",
          kind: "stalled",
        };
  }
  if (compacting) {
    return {
      text: "summarizing the history…",
      title: "the session's model is writing the summary",
      kind: "running",
    };
  }
  if (current.phase === "idle") return { text: "your turn", kind: "idle" };
  if (retrying) {
    return {
      text: "retrying the upstream · send to retry now",
      kind: "running",
    };
  }
  if (current.phase === "delayed" && view !== null) {
    const seconds = Math.max(0, Math.ceil((current.until - view.now) / 1000));
    return { text: `pacing · next step in ${seconds}s`, kind: "running" };
  }
  const operation =
    view === null ? null : operationText(view.operations, view.model, view.now);
  return {
    text: operation ?? "running…",
    title: view === null ? undefined : operationTitle(view.operations),
    kind: "running",
  };
}

export function turnClass(kind: TurnState["kind"]): string {
  switch (kind) {
    case "running":
      return "turn-state dim";
    case "stalled":
      return "turn-state stalled";
    case "parked":
      return "turn-state parked";
    case "waiting":
      return "turn-state waiting";
    case "idle":
      return "turn-state your-turn";
  }
}
