// The policy tab's rows (docs/web-client.md): the effective perimeter from
// the `role` topic, with pending proposals merged *in their committed sort
// position*. The rule carried from the TUI: committing a proposal must not
// move it — reflow at the moment of confirmation is how you lose track of
// what you confirmed — so a proposed row sorts by the same key its committed
// self will, and committing only drops the flag.

import type { ConfigChange, Proposal, Role, Seq } from "../api/types.ts";

export type PolicySection = "scope" | "commands" | "paths" | "tools";

export interface PolicyRow {
  section: PolicySection;
  text: string;
  /** The pending proposal this row stands for, `null` for a committed row. */
  proposal: Seq | null;
}

const SECTIONS: PolicySection[] = ["scope", "commands", "paths", "tools"];

const rw = (p: { read: boolean; write: boolean }) =>
  `${p.read ? "r" : ""}${p.write ? "w" : ""}`;

function changeRow(change: ConfigChange): {
  section: PolicySection;
  text: string;
} {
  if ("command" in change) {
    return { section: "commands", text: change.command.command };
  }
  if ("path" in change) {
    return { section: "paths", text: `${change.path.path} ${rw(change.path)}` };
  }
  if ("tool" in change) {
    return { section: "tools", text: change.tool.name };
  }
  return { section: "scope", text: change.scope.text };
}

/** All rows, grouped by section in a fixed order, sorted within a section by
 * display text — committed and proposed together, so a commit never reorders. */
export function policyRows(
  role: Role | null,
  proposals: Proposal[],
): PolicyRow[] {
  const rows: PolicyRow[] = [];
  if (role !== null) {
    rows.push({ section: "scope", text: role.scope, proposal: null });
    for (const command of role.commands) {
      rows.push({ section: "commands", text: command, proposal: null });
    }
    for (const [path, grant] of Object.entries(role.paths)) {
      rows.push({
        section: "paths",
        text: `${path} ${rw(grant)}`,
        proposal: null,
      });
    }
    for (const name of Object.keys(role.tools)) {
      rows.push({ section: "tools", text: name, proposal: null });
    }
  }
  for (const proposal of proposals) {
    rows.push({ ...changeRow(proposal.change), proposal: proposal.seq });
  }
  return rows.sort(
    (a, b) =>
      SECTIONS.indexOf(a.section) - SECTIONS.indexOf(b.section) ||
      a.text.localeCompare(b.text),
  );
}
