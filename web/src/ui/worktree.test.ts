import { describe, expect, it } from "vitest";
import { cleanWorktreeOutput, setupNote } from "./worktree.ts";

describe("the setup note", () => {
  it("is silent when the setup succeeded", () => {
    expect(setupNote(0)).toBe("");
  });

  it("states a failed setup for the line itself", () => {
    expect(setupNote(1)).toBe(" (setup failed: exit 1)");
    expect(setupNote(null)).toBe(" (setup failed: git ended by signal)");
  });
});

describe("the setup output cleaning", () => {
  it("keeps the last redraw of each progress line", () => {
    expect(
      cleanWorktreeOutput(
        "HEAD is now at b5a984a7 smoke\n" +
          "Preparing worktree\n" +
          "Updating files:  96% (1438/1485)\rUpdating files: 100% (1485/1485)\rUpdating files: 100% (1485/1485), done.\n",
      ),
    ).toBe(
      "HEAD is now at b5a984a7 smoke\n" +
        "Preparing worktree\n" +
        "Updating files: 100% (1485/1485), done.\n",
    );
  });

  it("tolerates a trailing carriage return and a missing newline", () => {
    expect(cleanWorktreeOutput("done.\r\n")).toBe("done.\n");
    expect(cleanWorktreeOutput("no newline")).toBe("no newline");
  });

  it("leaves already-clean output alone", () => {
    expect(cleanWorktreeOutput("hook failed\n")).toBe("hook failed\n");
    expect(cleanWorktreeOutput("")).toBe("");
  });
});
