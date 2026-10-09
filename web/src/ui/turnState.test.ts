import { describe, expect, it } from "vitest";
import type { CurrentOperations, Stall } from "../api/types.ts";
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

  describe("naming the leading operation", () => {
    const now = 1_754_300_130_000;
    const model = "z-ai/glm-5.3-flash";
    const view = (operations: CurrentOperations | null) => ({
      operations,
      model,
      now,
    });
    const ops = (
      requests: CurrentOperations["requests"],
      tools: CurrentOperations["tools"] = [],
    ): CurrentOperations => ({ requests, tools });
    const text = (operations: CurrentOperations | null) =>
      turnState(true, null, false, false, null, view(operations)).text;

    it("says running when nothing is open", () => {
      expect(text(null)).toBe("running…");
    });

    it("names the model a turn request is waiting on, with the elapsed time", () => {
      expect(
        text(
          ops([
            { request: 1, origin: "turn", stage: "sent", since: now - 130_000 },
          ]),
        ),
      ).toBe("waiting on z-ai/glm-5.3-flash · 2m 10s");
    });

    it("says streaming once the first byte arrived", () => {
      expect(
        text(
          ops([
            {
              request: 1,
              origin: "turn",
              stage: "streaming",
              since: now - 5_000,
            },
          ]),
        ),
      ).toBe("z-ai/glm-5.3-flash streaming");
    });

    it("names a refused attempt the transport is retrying", () => {
      expect(
        text(
          ops([
            {
              request: 1,
              origin: "turn",
              stage: "retrying",
              since: now - 8_000,
              attempt: 2,
              status: 429,
            },
          ]),
        ),
      ).toBe("retrying upstream · attempt 2 (429) · 8s");
    });

    it("names a peek and the spotter by what they are", () => {
      expect(
        text(
          ops([
            { request: 2, origin: "peek", stage: "sent", since: now - 40_000 },
          ]),
        ),
      ).toBe("triage peek · waiting 40s");
      expect(
        text(
          ops([
            { request: 3, origin: "spot", stage: "sent", since: now - 3_000 },
          ]),
        ),
      ).toBe("spotter judging · 3s");
    });

    it("names the running tool by its intent", () => {
      expect(
        text(
          ops(
            [],
            [
              {
                call_id: "t1",
                tool: "bash",
                intent: "run the suite",
                since: now - 45_000,
              },
            ],
          ),
        ),
      ).toBe("bash: run the suite · 45s");
    });

    it("leads with the turn's request, then a tool, then side requests", () => {
      const side = {
        request: 4,
        origin: "title" as const,
        stage: "sent" as const,
        since: now - 1_000,
      };
      const tool = {
        call_id: "t1",
        tool: "read",
        intent: null,
        since: now - 2_000,
      };
      const turn = {
        request: 5,
        origin: "turn" as const,
        stage: "sent" as const,
        since: now - 3_000,
      };
      expect(text(ops([side], [tool]))).toBe("read · 2s");
      expect(text(ops([side, turn], [tool]))).toContain("waiting on");
      expect(text(ops([side]))).toBe("writing a title · 1s");
    });

    it("keeps stall, park, fold and the retry wait ahead of the operation", () => {
      const open = view(
        ops([
          { request: 1, origin: "turn", stage: "sent", since: now - 1_000 },
        ]),
      );
      expect(turnState(true, stall, false, false, null, open).kind).toBe(
        "stalled",
      );
      expect(
        turnState(true, null, false, false, { reason: "folded" }, open).kind,
      ).toBe("parked");
      expect(turnState(true, null, false, true, null, open).text).toBe(
        "summarizing the history…",
      );
      expect(turnState(true, null, true, false, null, open).text).toBe(
        "retrying the upstream · send to retry now",
      );
    });

    it("does not say anything about operations while idle", () => {
      expect(
        turnState(
          false,
          null,
          false,
          false,
          null,
          view(
            ops([
              { request: 1, origin: "keepalive", stage: "sent", since: now },
            ]),
          ),
        ).text,
      ).toBe("your turn");
    });
  });
});
