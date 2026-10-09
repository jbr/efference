import { describe, expect, it } from "vitest";
import type { Cold } from "../api/types.ts";
import { foldOffer, foldWarning, kilotokens } from "./fold.ts";

const cold: Cold = {
  since: 1_754_300_000_000,
  context_tokens: 120_000,
};

describe("the fold offer", () => {
  it("is ready on an idle session, warm or cold", () => {
    expect(foldOffer(null, null, false, false, false)).toEqual({
      kind: "ready",
      warm: true,
    });
    expect(foldOffer(cold, null, false, false, false)).toEqual({
      kind: "ready",
      warm: false,
    });
  });

  it("reports a fold in flight before anything else", () => {
    expect(foldOffer(null, { state: "writing" }, true, true, true).kind).toBe(
      "writing",
    );
  });

  it("is ready after a failure, so the same button retries", () => {
    expect(
      foldOffer(cold, { state: "failed", error: "boom" }, false, false, false)
        .kind,
    ).toBe("ready");
  });

  it("is blocked by a gate or a question, naming which", () => {
    expect(foldOffer(cold, null, false, true, false).reason).toMatch(/gate/);
    expect(foldOffer(cold, null, false, false, true).reason).toMatch(
      /question/,
    );
  });

  it("is blocked while a warm loop runs, but not a cold one parked at its check", () => {
    expect(foldOffer(null, null, true, false, false).kind).toBe("blocked");
    expect(foldOffer(cold, null, true, false, false).kind).toBe("ready");
  });
});

describe("the fold wording", () => {
  it("says what a warm fold gives up and that there is no undo", () => {
    expect(foldWarning(null)).toMatch(/cache is live/);
    expect(foldWarning(null)).toMatch(/no undo/);
    expect(foldWarning(cold)).toMatch(/expired/);
  });

  it("rounds tokens to thousands", () => {
    expect(kilotokens(120_400)).toBe("120k");
  });
});
