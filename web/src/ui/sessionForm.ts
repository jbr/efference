// The new-session form's own logic (docs/web-client.md, backlog/new-session-form.md):
// what the operator has drafted, the request that draft makes, and how a
// preview reply is judged against it. Pure, so the rules are testable without a
// host and the component holds none of them.
//
// Two properties carry over from the terminal form (docs/tui.md) and are the
// reason this is separate from the component. **The draft is the operator's
// raw overrides and the host's answer never replaces it** — folding resolved
// values back in would turn every inherited default into an explicit override
// on the first preview — so `sessionRequest` sends the overrides and never the
// effective values. And **one request counter answers staleness**: a reply
// that lands after the draft moved on describes a draft nobody is looking at,
// so it is dropped rather than shown.

import type { ProjectEntry, SelectionSource } from "../api/sessionForm.ts";

/** Where a session would run. A location from the remembered index, or one
 * typed by hand; `config` null asks the host to probe, which is what a launch
 * that named none did. */
export interface Draft {
  cwd: string;
  config: string | null;
}

/** The body both `QUERY /sessions` and `POST /sessions` take. One conversion
 * feeds both, so the preview and the creation cannot drift.
 *
 * `env` is deliberately absent: the host establishes the environment from the
 * user-wide hook or its own, and a client cannot supply one worth supplying. */
export function sessionRequest(draft: Draft): Record<string, unknown> {
  return draft.config === null
    ? { cwd: draft.cwd }
    : { cwd: draft.cwd, config: draft.config };
}

/** The words each precedence branch is shown as. A field displays its source
 * because an inherited default and an explicit override are the same string
 * otherwise — "chosen" says the operator picked it, "your default" says they
 * did not, and the row is editable in place either way. */
const SOURCE_WORDS: Record<SelectionSource, string> = {
  request: "chosen",
  history: "a past pick",
  user_default: "your default",
  project_config: "project config",
  main_provider: "main's provider",
  decision_connection: "a decision model",
  provider_default: "provider default",
  shared_main: "shares main",
};

/** What a field says its value came from. An unrecognized branch (a host
 * ahead of this client) falls back to its own spelling rather than claiming a
 * source the client does not know. */
export function sourceWords(source: SelectionSource | null): string | null {
  return source === null ? null : (SOURCE_WORDS[source] ?? source);
}

/** The preview's own answer, narrowed to what this module reads. */
export interface PreviewAnswer {
  issues: { field: string; message: string }[];
}

/** A preview reply paired with the request counter it answers. A reply whose
 * counter is not the current one describes a draft that no longer exists, so
 * it is dropped rather than shown and the current draft is re-sent. */
export interface Answered {
  request: number;
  answer: PreviewAnswer;
}

/** The counter a committed edit moves to: one more than the last request, so
 * anything still in flight names an older draft. */
export const nextRequest = (current: number): number => current + 1;

/** Why `Start session` is not yet pressable, or `null` when it is.
 *
 * Refused while no preview has answered for the *current* draft, and while it
 * answered with issues: posting a creation the host is known to reject spends a
 * round trip to learn what is already on screen. */
export function startRefusal(
  draft: Draft | null,
  answered: Answered | null,
  current: number,
): string | null {
  if (draft === null) return "Pick a location";
  if (answered === null || answered.request !== current) return "Resolving…";
  if (answered.answer.issues.length > 0) return "Fix what is marked above";
  return null;
}

/** Whether the button may be pressed: the refusal above, and no creation in
 * flight. Creation is not idempotent, so a second press would mint a second
 * session — this is the duplicate-submit guard. */
export const canStart = (
  draft: Draft | null,
  answered: Answered | null,
  current: number,
  creating: boolean,
): boolean => !creating && startRefusal(draft, answered, current) === null;

/** The location a `+` on a workspace heading opens with: that workspace's
 * directory, carrying the remembered configuration when one was launched
 * there and `null` (probe) otherwise. A directory with no remembered entry is
 * still launchable — the index filters what creation would refuse, never what
 * it accepts. */
export function locationFor(
  projects: ProjectEntry[],
  cwd: string,
): Draft | null {
  return { cwd, config: recentFor(projects, cwd)?.config ?? null };
}

/** The most recent remembered entry for a directory, which is the first match:
 * the index is most-recently-used first. */
export const recentFor = (
  projects: ProjectEntry[],
  cwd: string,
): ProjectEntry | undefined => projects.find((entry) => entry.cwd === cwd);

/** What the top-bar entry opens with: the most recent remembered location,
 * which is the index's first entry, or `null` when nothing is remembered. */
export const mostRecent = (
  projects: ProjectEntry[],
): ProjectEntry | undefined => projects[0];

/** Whether a row says which configuration it would launch with. Shown when the
 * entry names one, and when two entries share a directory and would otherwise
 * be indistinguishable — the index keys an entry by directory *plus*
 * configuration, so one directory can be several rows. */
export function showsConfig(projects: ProjectEntry[], entry: ProjectEntry) {
  if (entry.config !== null) return true;
  return projects.filter((other) => other.cwd === entry.cwd).length > 1;
}

/** The rows as shown: the remembered index, and the draft's own directory ahead
 * of it when the index does not carry that one. A path typed into *Other
 * directory…* is a location like any other, so it stays on screen as the
 * chosen row rather than leaving the form looking as though nothing is
 * selected. The empty `config_key` is what marks it as not remembered, and so
 * not forgettable. */
export function withTypedLocation(
  projects: ProjectEntry[],
  draft: Draft | null,
): ProjectEntry[] {
  if (draft === null) return projects;
  if (projects.some((entry) => entry.cwd === draft.cwd)) return projects;
  return [
    {
      label: "",
      cwd: draft.cwd,
      config: draft.config,
      config_key: "",
      last_used_at: 0,
    },
    ...projects,
  ];
}
