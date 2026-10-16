import { describe, expect, it } from "vitest";
import type { Cold } from "../api/types.ts";
import { compactOffer, compactionWarning, kilotokens } from "./compact.ts";

const cold: Cold = {
  since: 1_754_300_000_000,
  context_tokens: 120_000,
};

describe("the compaction offer", () => {
  it("is ready on an idle session, warm or cold", () => {
    expect(compactOffer(null, null, false, false, false)).toEqual({
      kind: "ready",
      warm: true,
    });
    expect(compactOffer(cold, null, false, false, false)).toEqual({
      kind: "ready",
      warm: false,
    });
  });

  it("reports a compaction in flight before anything else", () => {
    expect(
      compactOffer(null, { state: "writing" }, true, true, true).kind,
    ).toBe("writing");
  });

  it("is ready after a failure, so the same button retries", () => {
    expect(
      compactOffer(
        cold,
        { state: "failed", error: "boom" },
        false,
        false,
        false,
      ).kind,
    ).toBe("ready");
  });

  it("is blocked by a gate or a question, naming which", () => {
    expect(compactOffer(cold, null, false, true, false).reason).toMatch(/gate/);
    expect(compactOffer(cold, null, false, false, true).reason).toMatch(
      /question/,
    );
  });

  it("is blocked while a warm loop runs, but not a cold one parked at its check", () => {
    expect(compactOffer(null, null, true, false, false).kind).toBe("blocked");
    expect(compactOffer(cold, null, true, false, false).kind).toBe("ready");
  });
});

describe("the compaction wording", () => {
  it("says what a warm compaction gives up and that there is no undo", () => {
    expect(compactionWarning(null)).toMatch(/cache is live/);
    expect(compactionWarning(null)).toMatch(/no undo/);
    expect(compactionWarning(cold)).toMatch(/expired/);
  });

  it("rounds tokens to thousands", () => {
    expect(kilotokens(120_400)).toBe("120k");
  });
});
