import { describe, expect, it } from "vitest";
import type { Role } from "../api/types.ts";
import { policyRows } from "./policy.ts";

const role = (commands: string[]): Role => ({
  source: null,
  scope: "work the efference tree",
  watch_for: [],
  commands,
  tools: {},
  paths: { ".": { read: true, write: true } },
});

describe("policy rows", () => {
  it("merges a proposal into its committed sort position", () => {
    const rows = policyRows(role(["cargo *", "ls *"]), [
      { seq: 17, change: { command: { command: "git status" } } },
    ]);
    const commands = rows.filter((r) => r.section === "commands");
    expect(commands.map((r) => r.text)).toEqual([
      "cargo *",
      "git status",
      "ls *",
    ]);
    expect(commands.map((r) => r.proposal)).toEqual([null, 17, null]);
  });

  it("committing does not move the row", () => {
    // The same grant, first as a proposal and then committed: same index.
    const before = policyRows(role(["cargo *", "ls *"]), [
      { seq: 17, change: { command: { command: "git status" } } },
    ]);
    const after = policyRows(role(["cargo *", "git status", "ls *"]), []);
    expect(before.findIndex((r) => r.text === "git status")).toBe(
      after.findIndex((r) => r.text === "git status"),
    );
  });

  it("orders sections fixed: scope, commands, paths, tools", () => {
    const rows = policyRows(role(["x"]), [
      { seq: 2, change: { tool: { name: "read" } } },
      { seq: 3, change: { path: { path: "/tmp", read: true, write: true } } },
    ]);
    expect(rows.map((r) => r.section)).toEqual([
      "scope",
      "commands",
      "paths",
      "paths",
      "tools",
    ]);
  });

  it("renders proposals alone when no role is set", () => {
    const rows = policyRows(null, [
      { seq: 5, change: { scope: { text: "new scope" } } },
    ]);
    expect(rows).toEqual([
      { section: "scope", text: "new scope", proposal: 5 },
    ]);
  });
});
