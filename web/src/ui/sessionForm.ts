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

import type {
  EffectiveTriage,
  ModelSelection,
  PreviewIssue,
  ProjectEntry,
  SelectionSource,
  SessionPreview,
} from "../api/sessionForm.ts";

/** What the operator has overridden, and where the session would run. A
 * location from the remembered index, or one typed by hand; `config` null asks
 * the host to probe, which is what a launch that named none did.
 *
 * Every optional field is the operator's *raw* override: absent means "inherit
 * whatever the host would have chosen", which is a different request from an
 * empty one. */
export interface Draft {
  cwd: string;
  config: string | null;
  /** The main role's selection, absent until the picker (step 3) sets one. */
  main: RoleDraft | null;
  spotter: RoleDraft | null;
  triage: TriageDraft;
  /** Effort level, `null` for the provider's own. */
  effort: string | null;
  /** Sampling temperature, `null` for the provider's own. */
  temperature: number | null;
}

/** One role's selection: the connection by name, and the provider-native model
 * id under it. Two fields rather than a `connection/model` slug, because that
 * is what the request shape names and what keeps a native id containing a slash
 * unambiguous (docs/control-api.md). */
export interface RoleDraft {
  connection: string;
  model: string;
}

/** Triage either shares the main client or names its own. Inheriting sends
 * nothing, so an untouched triage resolves to sharing main. */
export type TriageDraft =
  { mode: "inherit" } | ({ mode: "separate" } & RoleDraft);

/** An empty draft: a session with nothing overridden, which is what the host
 * resolves on its own. */
export const emptyDraft = (
  cwd: string,
  config: string | null = null,
): Draft => ({
  cwd,
  config,
  main: null,
  spotter: null,
  triage: { mode: "inherit" },
  effort: null,
  temperature: null,
});

/** The body both `QUERY /sessions` and `POST /sessions` take. One conversion
 * feeds both, so the preview and the creation cannot drift.
 *
 * `env` is deliberately absent: the host establishes the environment from the
 * user-wide hook or its own, and a client cannot supply one worth supplying. */
export function sessionRequest(draft: Draft): Record<string, unknown> {
  return {
    cwd: draft.cwd,
    ...(draft.config === null ? {} : { config: draft.config }),
    ...roleRequest("main", draft.main),
    ...roleRequest("spotter", draft.spotter),
    ...(draft.triage.mode === "inherit"
      ? {}
      : {
          triage: {
            mode: "separate",
            connection: draft.triage.connection,
            model: draft.triage.model,
          },
        }),
    ...(draft.effort === null ? {} : { effort: draft.effort }),
    ...(draft.temperature === null ? {} : { temperature: draft.temperature }),
  };
}

/** A role names its connection and model separately, or sends nothing — a slug
 * is also accepted by the wire, but two fields keep an id that contains a
 * slash unambiguous (docs/control-api.md).
 *
 * Main is the one role the wire spells without a prefix; only the other roles
 * carry one, so a `main_` prefix would be a field the host does not read, and
 * the override would be dropped in silence. */
const roleRequest = (role: "main" | "spotter", draft: RoleDraft | null) => {
  if (draft === null) return {};
  const prefix = role === "main" ? "" : "spotter_";
  return {
    [`${prefix}connection`]: draft.connection,
    [`${prefix}model`]: draft.model,
  };
};

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

/** What one settings row says beside its value. A field the host could not
 * resolve has no source *and* an issue naming why, so the issue's message takes
 * the source's place rather than sitting beside it — the row reads as one
 * answer, not two. */
export function fieldSource(
  field: string,
  source: SelectionSource | null,
  issues: PreviewIssue[],
): string | null {
  const issue = issues.find((entry) => entry.field === field);
  return issue === undefined ? sourceWords(source) : issue.message;
}

/** Whether a field's problem is one that refuses Start, as opposed to the whole
 * preview failing to arrive. */
export const hasIssue = (issues: PreviewIssue[], field: string): boolean =>
  issues.some((entry) => entry.field === field);

/** The request fields the Settings rows carry an issue on. */
export const SETTING_FIELDS = [
  "model",
  "spotter_model",
  "triage",
  "effort",
  "temperature",
];

/** The issues that belong to no row, and so are stated beside Start instead.
 * A field with a row says its own problem where the operator is editing it;
 * repeating it under the button states the same thing twice. One issue has no
 * row: a configuration that will not load belongs to the location, which is
 * displayed rather than edited. */
export const orphanIssues = (issues: PreviewIssue[]): PreviewIssue[] =>
  issues.filter((issue) => !SETTING_FIELDS.includes(issue.field));

/** A resolved model as its row shows it: `connection/model`, the slug a reader
 * recognizes, unambiguous because the connection is named beside the id. */
export const modelText = (value: ModelSelection | null): string | null =>
  value === null ? null : `${value.connection}/${value.model}`;

/** Triage's own value. Sharing main is what its *source* says, so the value
 * cell stays empty rather than repeating the main model in it. */
export const triageText = (value: EffectiveTriage | null): string | null =>
  value === null || value.mode === "shared"
    ? null
    : `${value.connection}/${value.model}`;

/** The effort levels the terminal form offers (`src/tui/session_form.rs`), in
 * its order. The first is the absence of an override, not a level: the wire
 * takes no `effort` and the provider decides. */
export const EFFORTS: { label: string; value: string | null }[] = [
  { label: "Use default", value: null },
  { label: "low", value: "low" },
  { label: "medium", value: "medium" },
  { label: "high", value: "high" },
  { label: "xhigh", value: "xhigh" },
  { label: "max", value: "max" },
];

/** What a temperature editor holds, committed. Empty is the default, and a
 * value outside the accepted 0–100 is sent as typed rather than clamped: the
 * host reports it as an issue, which refuses Start and names itself — a silent
 * clamp would launch a session at a temperature the operator did not ask for. */
export function temperatureOf(text: string): number | null {
  const trimmed = text.trim();
  if (trimmed === "") return null;
  const value = Number(trimmed);
  return Number.isInteger(value) ? value : null;
}

/** A preview reply paired with the request counter it answers. A reply whose
 * counter is not the current one describes a draft that no longer exists, so
 * it is dropped rather than shown and the current draft is re-sent. */
export interface Answered {
  request: number;
  answer: SessionPreview;
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
export function locationFor(projects: ProjectEntry[], cwd: string): Location {
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

/** The part of a draft that says where the session runs: picking a location is
 * one edit, and it does not undo the settings chosen beside it. */
export type Location = Pick<Draft, "cwd" | "config">;

/** The draft a clicked row stands for: **the row's own configuration**, not
 * the directory's most recently used one. The index keys an entry by directory
 * *plus* configuration, so one directory is several rows, and picking the
 * second must select the second — reading the directory instead would select a
 * row the operator did not click, and leave neither row highlighted. */
export const draftFor = (entry: ProjectEntry): Location => ({
  cwd: entry.cwd,
  config: entry.config,
});

/** The key one row is rendered under. `config_key` alone collides: a directory
 * launched without a configuration is stored under the same `builtin-default`
 * as every other such directory. */
export const rowKey = (entry: ProjectEntry): string =>
  `${entry.cwd} ${entry.config_key}`;

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
