import { describe, expect, it } from "vitest";
import type { Stall } from "../api/types.ts";
import { turnState } from "./turnState.ts";

const stall: Stall = {
  reason: "retries_exhausted",
  attempts: 12,
  at: 1_754_300_000_000,
  message: "the trajectory stopped: 12 attempts in a row failed upstream",
  detail: "upstream SSE stream failed mid-stream: connection reset",
};

describe("the turn-state readout", () => {
  it("says whose turn it is while nothing is wrong", () => {
    expect(turnState(false, null)).toEqual({ text: "your turn", kind: "idle" });
    expect(turnState(true, null).kind).toBe("running");
  });

  it("distinguishes an idle daemon that gave up from one that finished", () => {
    const state = turnState(false, stall);
    expect(state.kind).toBe("stalled");
    expect(state.text).toBe(`${stall.message} · send to retry`);
    expect(state.title).toBe(stall.detail);
  });

  it("reports a stall while running — a triage gate held on the upstream", () => {
    expect(turnState(true, stall).kind).toBe("stalled");
  });

  it("reads a fold in flight as running, over an idle loop", () => {
    const state = turnState(false, null, false, true);
    expect(state.kind).toBe("running");
    expect(state.text).toBe("summarizing the history…");
  });

  it("reads a loop parked at the cold check as waiting on the operator", () => {
    const cold = {
      reason: "expired" as const,
      since: 1,
      context_tokens: 120_400,
    };
    const state = turnState(true, null, false, false, cold);
    expect(state.kind).toBe("parked");
    expect(state.text).toBe(
      "paused: cache expired · send to continue (reads 120k tokens uncached)",
    );
    expect(
      turnState(true, null, false, false, { ...cold, context_tokens: null })
        .text,
    ).toContain("the whole context");
  });

  it("reads a park after a fold as resuming without consent", () => {
    const state = turnState(true, null, false, false, { reason: "folded" });
    expect(state.kind).toBe("parked");
    expect(state.text).toBe("paused: summarized · send to continue");
  });

  it("does not read an unparked session, or a fold over a park, as parked", () => {
    expect(turnState(false, null, false, false, null).kind).toBe("idle");
    expect(turnState(true, null, false, false, null).kind).toBe("running");
    expect(turnState(true, null, false, true, { reason: "folded" }).text).toBe(
      "summarizing the history…",
    );
  });

  it("says a retry wait can be cut short by an empty send", () => {
    const state = turnState(true, null, true);
    expect(state.kind).toBe("running");
    expect(state.text).toBe("retrying the upstream · send to retry now");
  });
});
