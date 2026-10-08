import { describe, expect, it } from "vitest";
import { gapText, resumedLine } from "./resumed.ts";

describe("the resumed notice", () => {
  it("writes the gap as the wire notice does", () => {
    expect(gapText(45_000)).toBe("45s");
    expect(gapText(12 * 60_000)).toBe("12m");
    expect(gapText(3 * 3_600_000 + 5 * 60_000)).toBe("3h 5m");
    expect(gapText(2 * 86_400_000 + 4 * 3_600_000)).toBe("2d 4h");
  });

  it("states a clean restart plainly", () => {
    expect(resumedLine({ gap_ms: 300_000 }, 50)).toBe(
      "session resumed after 5m: the host process restarted.",
    );
  });

  it("names lost processes, pending calls and skipped entries", () => {
    const line = resumedLine(
      {
        gap_ms: 1_000,
        lost_processes: ["p#1", "p#2"],
        pending_calls: ["a", "b"],
        resumes_after: 40,
      },
      44,
    );
    expect(line).toContain("p#1, p#2");
    expect(line).toContain("2 tool calls pending");
    expect(line).toContain("3 entries after seq 40 were not restored");
  });

  it("does not claim skipped entries when none were", () => {
    expect(resumedLine({ gap_ms: 0, resumes_after: 49 }, 50)).not.toContain(
      "did not end cleanly",
    );
  });
});
