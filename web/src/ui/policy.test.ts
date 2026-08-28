import { describe, expect, it } from "vitest";
import type { Role, CommandStance } from "../api/types.ts";
import { policyRows, toolDisplayName } from "./policy.ts";

const role = (
  commands: Record<string, CommandStance>,
  tools: string[] = [],
): Role => ({
  source: null,
  scope: "work the efference tree",
  watch_for: [],
  commands,
  tools,
  paths: { ".": { read: true, write: true } },
});

describe("policy rows", () => {
  it("merges a proposal into its committed sort position", () => {
    const rows = policyRows(role({ "cargo *": "clear", "ls *": "clear" }), [
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
    const before = policyRows(role({ "cargo *": "clear", "ls *": "clear" }), [
      { seq: 17, change: { command: { command: "git status" } } },
    ]);
    const after = policyRows(
      role({ "cargo *": "clear", "git status": "clear", "ls *": "clear" }),
      [],
    );
    expect(before.findIndex((r) => r.text === "git status")).toBe(
      after.findIndex((r) => r.text === "git status"),
    );
  });

  it("reads the stance from the commands object, not a plain list", () => {
    const rows = policyRows(role({ git: "clear", "git push": "deny" }), []);
    const commands = rows.filter((r) => r.section === "commands");
    expect(commands).toEqual([
      { section: "commands", text: "git", stance: "clear", proposal: null },
      {
        section: "commands",
        text: "git push",
        stance: "deny",
        proposal: null,
      },
    ]);
  });

  it("renders tools from the wire array through the dotted display mapping", () => {
    const rows = policyRows(
      role({}, ["edit", "ferritin__get", "ferritin__*"]),
      [],
    );
    const tools = rows.filter((r) => r.section === "tools");
    expect(tools.map((r) => r.text)).toEqual([
      "edit",
      "ferritin.*",
      "ferritin.get",
    ]);
  });

  it("orders sections fixed: scope, commands, paths, tools", () => {
    const rows = policyRows(role({ x: "clear" }), [
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
      { section: "scope", text: "new scope", stance: null, proposal: 5 },
    ]);
  });
});

describe("toolDisplayName", () => {
  it("dots the namespace boundary at the first `__` only", () => {
    expect(toolDisplayName("ferritin__get")).toBe("ferritin.get");
    expect(toolDisplayName("edit")).toBe("edit");
    expect(toolDisplayName("ferritin__get__raw")).toBe("ferritin.get__raw");
  });

  it("dots the whole-namespace glob the same way", () => {
    expect(toolDisplayName("ferritin__*")).toBe("ferritin.*");
  });
});
