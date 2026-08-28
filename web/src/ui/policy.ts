// The policy tab's rows (docs/web-client.md): the effective perimeter from
// the `role` topic, with pending proposals merged *in their committed sort
// position*. The rule carried from the TUI: committing a proposal must not
// move it — reflow at the moment of confirmation is how you lose track of
// what you confirmed — so a proposed row sorts by the same key its committed
// self will, and committing only drops the flag.

import type {
  ConfigChange,
  Proposal,
  Role,
  Seq,
  CommandStance,
} from "../api/types.ts";
import { splitToolName } from "../render/parse.ts";

export type PolicySection = "scope" | "commands" | "paths" | "tools";

export interface PolicyRow {
  section: PolicySection;
  text: string;
  /** Set only for `commands` rows — the stance a `clear` row still carries,
   * so a carve-out is distinguishable from a grant at render time. */
  stance: CommandStance | null;
  /** The pending proposal this row stands for, `null` for a committed row. */
  proposal: Seq | null;
}

const SECTIONS: PolicySection[] = ["scope", "commands", "paths", "tools"];

const rw = (p: { read: boolean; write: boolean }) =>
  `${p.read ? "r" : ""}${p.write ? "w" : ""}`;

/** Mirrors `mcp::display_name` (src/mcp.rs): dot the namespace boundary at
 * the *first* `__` only, so a server's own `__`-bearing tool name (or the
 * whole-namespace glob `ferritin__*`) still round-trips as one substitution.
 * A bare built-in name (no `__`) passes through unchanged — it is not given
 * an `efference.` prefix on either client. */
export function toolDisplayName(wireName: string): string {
  const { server, tool } = splitToolName(wireName);
  return server === null ? tool : `${server}.${tool}`;
}

function changeRow(change: ConfigChange): {
  section: PolicySection;
  text: string;
  stance: CommandStance | null;
} {
  if ("command" in change) {
    // Prefix-promotion files clear grants only (docs/permissions.md) — a
    // proposal can never carry a carve-out stance.
    return {
      section: "commands",
      text: change.command.command,
      stance: "clear",
    };
  }
  if ("path" in change) {
    return {
      section: "paths",
      text: `${change.path.path} ${rw(change.path)}`,
      stance: null,
    };
  }
  if ("tool" in change) {
    return {
      section: "tools",
      text: toolDisplayName(change.tool.name),
      stance: null,
    };
  }
  return { section: "scope", text: change.scope.text, stance: null };
}

/** All rows, grouped by section in a fixed order, sorted within a section by
 * display text — committed and proposed together, so a commit never reorders. */
export function policyRows(
  role: Role | null,
  proposals: Proposal[],
): PolicyRow[] {
  const rows: PolicyRow[] = [];
  if (role !== null) {
    rows.push({
      section: "scope",
      text: role.scope,
      stance: null,
      proposal: null,
    });
    for (const [command, stance] of Object.entries(role.commands)) {
      rows.push({ section: "commands", text: command, stance, proposal: null });
    }
    for (const [path, grant] of Object.entries(role.paths)) {
      rows.push({
        section: "paths",
        text: `${path} ${rw(grant)}`,
        stance: null,
        proposal: null,
      });
    }
    for (const name of role.tools) {
      rows.push({
        section: "tools",
        text: toolDisplayName(name),
        stance: null,
        proposal: null,
      });
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
