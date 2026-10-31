// What a turn's `source` label says (docs/control-api.md): who sent it, as the
// transcript names them. The label is a string on the wire; these read the forms
// the host writes (src/frontend.rs).

/** The source the harness's `first-turn` kickoff carries (src/frontend.rs). The
 * turn belongs to the session's opening, like the preamble, so the transcript
 * does not draw it (docs/context.md). */
export const KICKOFF_SOURCE = "efference";

/** The predecessor a hand-off brief's `source` names — `handoff[<id>]`, the
 * reserved form `hand_off` writes (src/frontend.rs, docs/tools.md) — or `null`
 * for any other source. The brief is the predecessor speaking, not the operator. */
export function handoffPredecessor(
  source: string | null | undefined,
): string | null {
  const match = /^handoff\[(.+)\]$/.exec(source ?? "");
  return match === null ? null : match[1];
}

/** A `session[…]` source's id, and the project it names when the sending
 * session worked in another project than the recipient (src/frontend.rs,
 * `session_source`). An id holds no space, so the first ` in ` ends it. */
export function sessionSource(
  source: string,
): { id: string; place: string | null } | null {
  const match = /^session\[([^\s\]]+)(?: in (.+))?\]$/.exec(source);
  return match === null ? null : { id: match[1], place: match[2] ?? null };
}

/** Who sent a turn, as the transcript names them (src/daemon/transcript.rs,
 * `sender`): `you` for the operator's own clients (no source, or `web`), and
 * otherwise the id or client name inside its `source` label — `parent` for
 * `session[parent]` and `session[parent in ~/code/other]`, `claude-code` for
 * `mcp[claude-code]`. */
export function senderName(source: string | null | undefined): string {
  if (source === null || source === undefined || source === "web") return "you";
  const session = sessionSource(source);
  if (session !== null) return session.id;
  const match = /^(?:agent|mcp)\[(.+)\]$/.exec(source);
  return match === null ? source : match[1];
}
