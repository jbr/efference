import { describe, expect, it } from "vitest";
import type { Cold, CurrentOperations, Verbs } from "../api/types.ts";
import { compactOffer, kilotokens } from "./compact.ts";

const cold: Cold = {
  since: 1_754_300_000_000,
  context_tokens: 120_000,
};

const verbs = (compact: Verbs["compact"]): Verbs => ({
  turn: { accepted: true, delivery: "now" },
  continue: { accepted: true },
  compact,
});

const writing: CurrentOperations = {
  requests: [],
  tools: [],
  compaction: { state: "writing", warm: true, cut: 12, since: 1 },
};

describe("the compaction offer", () => {
  it("is the compact route's published answer", () => {
    expect(
      compactOffer(verbs({ accepted: true, warm: false }), null, cold),
    ).toEqual({ kind: "ready", warm: false });
    expect(
      compactOffer(verbs({ accepted: true, warm: true }), null, null),
    ).toEqual({ kind: "ready", warm: true });
    expect(
      compactOffer(
        verbs({
          accepted: false,
          message: "an approval gate is pending — resolve it first",
        }),
        null,
        cold,
      ),
    ).toEqual({
      kind: "blocked",
      reason: "an approval gate is pending — resolve it first",
      warm: false,
    });
  });

  it("reports a compaction in flight before anything else", () => {
    expect(
      compactOffer(verbs({ accepted: false, message: "busy" }), writing, cold),
    ).toEqual({ kind: "writing", warm: true });
  });

  it("leaves the answer to the route before the topic lands", () => {
    expect(compactOffer(null, null, cold)).toEqual({
      kind: "ready",
      warm: false,
    });
    expect(compactOffer(null, null, null).warm).toBe(true);
  });
});

describe("the compaction wording", () => {
  it("rounds tokens to thousands", () => {
    expect(kilotokens(120_400)).toBe("120k");
  });
});
