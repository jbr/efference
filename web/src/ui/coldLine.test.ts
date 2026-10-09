import { describe, expect, it } from "vitest";
import type { Cold } from "../api/types.ts";
import { coldLine } from "./coldLine.ts";

const cold: Cold = { since: 1_754_300_000_000, context_tokens: 120_400 };

describe("the cold line at a decision", () => {
  it("states the cost of approving where it is known", () => {
    expect(coldLine(cold, "approving or redirecting")).toBe(
      "cache expired — approving or redirecting reads ~120k tokens uncached",
    );
  });

  it("states the cost of an answer", () => {
    expect(coldLine(cold, "your answer")).toBe(
      "cache expired — your answer reads ~120k tokens uncached",
    );
  });

  it("still says the read is uncached when the cost is unknown", () => {
    expect(coldLine({ ...cold, context_tokens: null }, "your answer")).toBe(
      "cache expired — your answer reads the context uncached",
    );
  });

  it("is absent while the cache is not believed expired", () => {
    expect(coldLine(null, "your answer")).toBeNull();
  });
});

describe("the composer's line", () => {
  it("states what a send reads", () => {
    expect(coldLine(cold, "a send")).toBe(
      "cache expired — a send reads ~120k tokens uncached",
    );
  });
});
