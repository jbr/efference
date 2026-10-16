import { describe, expect, it } from "vitest";
import type { CurrentOperations, Park, Stall } from "../api/types.ts";
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

  it("reports a stall while running — a triage held on the upstream", () => {
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

  it("reads a park after a compaction as resuming", () => {
    const state = turnState(true, null, false, false, { reason: "compacted" });
    expect(state.kind).toBe("parked");
    expect(state.text).toBe("paused: summarized · send to continue");
  });

  it("does not read an unparked session, or a compaction over a park, as parked", () => {
    expect(turnState(false, null, false, false, null).kind).toBe("idle");
    expect(turnState(true, null, false, false, null).kind).toBe("running");
    expect(
      turnState(true, null, false, true, { reason: "compacted" }).text,
    ).toBe("summarizing the history…");
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

    it("names a triage and the spotter by what they are", () => {
      expect(
        text(
          ops([
            {
              request: 2,
              origin: "triage",
              stage: "sent",
              since: now - 40_000,
            },
          ]),
        ),
      ).toBe("triage · waiting 40s");
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

    it("names a bash call by its command, with the intent as the hover title", () => {
      const tool = {
        call_id: "t1",
        tool: "bash",
        intent: "Operator asked for a foreground UI test",
        command: "sleep 25 && echo done",
        since: now - 15_000,
      };
      const state = turnState(
        true,
        null,
        false,
        false,
        null,
        view(ops([], [tool])),
      );
      expect(state.text).toBe("bash: sleep 25 && echo done · 15s");
      expect(state.title).toBe("Operator asked for a foreground UI test");
      const long = { ...tool, command: "x".repeat(60) };
      expect(text(ops([], [long]))).toBe(`bash: ${"x".repeat(47)}… · 15s`);
      const plain = { ...tool, tool: "read", command: undefined };
      expect(text(ops([], [plain]))).toBe(
        "read: Operator asked for a foreground UI test · 15s",
      );
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

    it("keeps stall, park, compaction and the retry wait ahead of the operation", () => {
      const open = view(
        ops([
          { request: 1, origin: "turn", stage: "sent", since: now - 1_000 },
        ]),
      );
      expect(turnState(true, stall, false, false, null, open).kind).toBe(
        "stalled",
      );
      expect(
        turnState(true, null, false, false, { reason: "compacted" }, open).kind,
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

  it("reads a pending gate or ask as waiting on the operator, over any operation", () => {
    const operations: CurrentOperations = {
      requests: [{ request: 4, origin: "title", stage: "sent", since: 1 }],
      tools: [],
    };
    const view = { operations, model: "m", now: 26_000 };
    const gate = turnState(true, null, false, false, null, view, "gate");
    expect(gate).toEqual({
      text: "waiting for your approval",
      kind: "waiting",
    });
    const ask = turnState(true, null, false, false, null, view, "ask");
    expect(ask.text).toBe("waiting for your answer");
    // A stall and a park still come first.
    expect(turnState(true, stall, false, false, null, view, "gate").kind).toBe(
      "stalled",
    );
    const park: Park = { reason: "compacted" };
    expect(turnState(true, null, false, false, park, view, "gate").kind).toBe(
      "parked",
    );
  });
});
