import { describe, expect, it } from "vitest";
import type { Cold } from "../api/types.ts";
import {
  coldDecisionLine,
  consentsWhenShown,
  decisionWithConsent,
} from "./coldConsent.ts";

const cold: Cold = { since: 1_754_300_000_000, context_tokens: 120_400 };

describe("the cold line at a decision", () => {
  it("states the cost of approving where it is known", () => {
    expect(coldDecisionLine(cold, "approving or redirecting")).toBe(
      "cache believed cold — approving or redirecting reads ~120k tokens uncached",
    );
  });

  it("states the cost of an answer", () => {
    expect(coldDecisionLine(cold, "your answer")).toBe(
      "cache believed cold — your answer reads ~120k tokens uncached",
    );
  });

  it("still says the read is uncached when the cost is unknown", () => {
    expect(
      coldDecisionLine({ ...cold, context_tokens: null }, "your answer"),
    ).toBe("cache believed cold — your answer reads the context uncached");
  });

  it("is absent while the cache is not believed expired", () => {
    expect(coldDecisionLine(null, "your answer")).toBeNull();
  });
});

describe("when a decision carries the consent", () => {
  const line = coldDecisionLine(cold, "approving or redirecting");

  it("sends the flag with an approval, a redirect and a grant when the line showed", () => {
    expect(decisionWithConsent({ decision: "approve" }, line)).toEqual({
      decision: "approve",
      accept_cold_read: true,
    });
    expect(
      decisionWithConsent({ decision: "redirect", message: "no" }, line),
    ).toEqual({ decision: "redirect", message: "no", accept_cold_read: true });
  });

  it("sends nothing extra when no line showed", () => {
    expect(decisionWithConsent({ decision: "approve" }, null)).toEqual({
      decision: "approve",
    });
    expect(consentsWhenShown(null)).toBe(false);
  });

  it("leaves an abort alone", () => {
    expect(decisionWithConsent({ decision: "abort" }, line)).toEqual({
      decision: "abort",
    });
  });

  it("consents for an answer exactly when the ask's line showed", () => {
    expect(consentsWhenShown(coldDecisionLine(cold, "your answer"))).toBe(true);
    expect(consentsWhenShown(coldDecisionLine(null, "your answer"))).toBe(
      false,
    );
  });
});
