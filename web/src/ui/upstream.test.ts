import { describe, expect, it } from "vitest";
import type { UpstreamRetry } from "../api/types.ts";
import { upstreamChip } from "./upstream.ts";

const NOW = 1_754_300_000_000;

const retry = (
  surface: UpstreamRetry["surface"],
  attempt: number,
): UpstreamRetry => ({
  surface,
  attempt,
  budget: 12,
  next_attempt_at: NOW + 4_000,
  message: `stream aborted, retrying (${attempt} of 12)`,
  detail: "upstream SSE stream failed mid-stream: connection reset",
});

describe("the upstream chip", () => {
  it("is absent when neither topic is set — the all-clear", () => {
    expect(upstreamChip(null, null)).toBeNull();
  });

  it("names the last non-success status, eliding a count of one", () => {
    expect(upstreamChip({ status: 429, count: 1 }, null)?.text).toBe(
      "upstream 429",
    );
    expect(upstreamChip({ status: 429, count: 3 }, null)?.text).toBe(
      "upstream 429 ×3",
    );
  });

  it("leads with the retry message, not the status, while one is in flight", () => {
    const chip = upstreamChip({ status: 529, count: 2 }, retry("turn", 2));
    expect(chip?.text).toBe("↻ stream aborted, retrying (2 of 12)");
    expect(chip?.retrying).toBe(true);
    expect(chip?.text).not.toContain("529");
  });

  it("marks a fork peek's retry apart from a turn's", () => {
    expect(upstreamChip(null, retry("peek", 3))?.text).toBe(
      "↻ peek: stream aborted, retrying (3 of 12)",
    );
  });

  // The chip is a pure function of the two topics — no clock is read, so it
  // never renders a stale countdown.
  it("puts the upstream's own words in the title", () => {
    expect(upstreamChip(null, retry("turn", 2))?.title).toBe(
      "upstream SSE stream failed mid-stream: connection reset",
    );
  });

  it("falls back to the failure chip once the retry clears", () => {
    expect(upstreamChip({ status: 529, count: 2 }, null)?.text).toBe(
      "upstream 529 ×2",
    );
  });
});
