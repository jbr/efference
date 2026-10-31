// The restart notice a `Resumed` entry carries (docs/resumption.md), in the words
// the model reads for it (`render_resumed`, src/project/render.rs): the same facts
// stated to the operator, minus the leading glyph the wire notice has.

import type { Resumed } from "../api/types.ts";
import type { Seq } from "../api/entry.ts";

/** A gap as the wire notice writes it: `45s`, `12m`, `3h 5m`, `2d 4h`. */
export function gapText(gapMs: number): string {
  const secs = Math.floor(gapMs / 1000);
  if (secs < 60) return `${secs}s`;
  if (secs < 3600) return `${Math.floor(secs / 60)}m`;
  if (secs < 86400) {
    return `${Math.floor(secs / 3600)}h ${Math.floor((secs % 3600) / 60)}m`;
  }
  return `${Math.floor(secs / 86400)}d ${Math.floor((secs % 86400) / 3600)}h`;
}

/** The notice for a `Resumed` entry at `at`. */
export function resumedLine(resumed: Resumed, at: Seq): string {
  let out = `session resumed after ${gapText(resumed.gap_ms)}: the host process restarted.`;
  const lost = resumed.lost_processes ?? [];
  if (lost.length > 0) {
    out +=
      " Background processes no longer monitored, output no longer captured, " +
      `still running or not unknown: ${lost.join(", ")}.`;
  }
  const pending = resumed.pending_calls?.length ?? 0;
  if (pending > 0) {
    const [noun, pronoun] = pending === 1 ? ["call", "it"] : ["calls", "them"];
    out +=
      ` The session stopped with ${pending} tool ${noun} pending; the ` +
      `operator's next action brings ${pronoun} back.`;
  }
  const after = resumed.resumes_after;
  if (after != null && after + 1 < at) {
    const count = at - (after + 1);
    const [noun, were, they] =
      count === 1
        ? ["entry", "was", "it may include a tool call that ran"]
        : ["entries", "were", "they may include tool calls that ran"];
    out +=
      ` The session did not end cleanly: ${count} ${noun} after seq ${after} ` +
      `${were} not restored, and ${they}.`;
  }
  return out;
}
