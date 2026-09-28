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

  it("says a retry wait can be cut short by an empty send", () => {
    const state = turnState(true, null, true);
    expect(state.kind).toBe("running");
    expect(state.text).toBe("retrying the upstream · send to retry now");
  });
});
