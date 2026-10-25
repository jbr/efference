// Switching a session's models at a restart (docs/resumption.md): the page's
// draft, the roles it compares against, and the `active` patch fields it
// becomes. Effort and temperature are absent because they come from the header
// the session is resumed with, and so is location, which stays where the
// session lives. A role the draft leaves alone keeps what the log records.
//
// One draft serves both states a session can be in. A running session is
// archived and then resumed on the chosen models; a dormant one resumes on them
// in a single patch, seeded from the roles its listing read back from the log.

import type { ArchivedSession, DormantSession } from "../api/http.ts";
import type {
  Cold,
  RecordedSelections,
  SessionIdentity,
} from "../api/types.ts";

/** One role's selection: the connection by name and the provider-native model id
 * under it, in the shape the picker reports a choice. */
export interface Selection {
  connection: string;
  model: string;
}

/** What the page compares a pick against: the connection and the model id. The
 * identity's `ModelSelection` also carries the provider kind, so it satisfies
 * this shape, and so does a role read back from a dormant session's log. */
export interface RecordedSelection {
  connection: string;
  model: string;
}

/** Every role as the session last ran on it. A role the log does not record is
 * absent: a resume that leaves it out keeps it, so the page can still move the
 * others. */
export interface RecordedRoles {
  main?: RecordedSelection;
  spotter?: RecordedSelection;
  triage?: RecordedSelection & { shared: boolean };
}

/** What the page is switching. */
export interface SwitchTarget {
  id: string;
  /** The root the resume patch must name: the host serves several. */
  cwd: string | null;
  /** Whether the session is running now: a live one is archived before it is
   * resumed, and a dormant one only resumes. */
  live: boolean;
  recorded: RecordedRoles | null;
  /** The `cold` topic, which only a running session has. */
  cold: Cold | null;
  /** What the context holds now, when a usage record says. */
  contextTokens: number | null;
}

/** A running session, as its own view knows it. */
export function liveTarget(
  id: string,
  identity: SessionIdentity | null,
  cold: Cold | null,
  contextTokens: number | null,
): SwitchTarget {
  return {
    id,
    cwd: identity?.cwd ?? null,
    live: true,
    recorded: identity?.inference ?? null,
    cold,
    contextTokens,
  };
}

/** A dormant session, as the overview lists it. `read` is the same session's
 * single-row answer, when the caller already has it; with none, the page fetches
 * it, since the listing carries no roles. */
export function archivedTarget(
  session: ArchivedSession,
  read: DormantSession | null = null,
): SwitchTarget {
  return {
    id: session.id,
    cwd: session.cwd,
    live: false,
    recorded: read === null ? null : recordedFromSelections(read.selections),
    cold: null,
    contextTokens: read?.context_tokens ?? null,
  };
}

/** The roles an archived row's `selections` names. A role it does not name stays
 * absent, so its row opens on "Choose a model" and a pick is what the resume
 * sends for it. */
export function recordedFromSelections(
  selections: RecordedSelections | null | undefined,
): RecordedRoles | null {
  if (selections == null) return null;
  const roles: RecordedRoles = {};
  if (selections.connection && selections.model) {
    roles.main = { connection: selections.connection, model: selections.model };
  }
  if (selections.spotter_connection && selections.spotter_model) {
    roles.spotter = {
      connection: selections.spotter_connection,
      model: selections.spotter_model,
    };
  }
  const triage = selections.triage;
  if (triage?.mode === "shared") {
    // A shared triage repeats main's selection; without one there is nothing to
    // repeat, and the role stays unknown rather than guessed.
    if (roles.main !== undefined)
      roles.triage = { ...roles.main, shared: true };
  } else if (triage?.mode === "separate") {
    // A separate switch names the connection, or leaves it out and gives
    // `connection/model` as the model. Split at the first slash: a connection
    // name holds none, while the native id under it can.
    const selection = triage.connection
      ? { connection: triage.connection, model: triage.model }
      : splitSlug(triage.model);
    if (selection !== null) roles.triage = { ...selection, shared: false };
  }
  return roles;
}

function splitSlug(slug: string): RecordedSelection | null {
  const cut = slug.indexOf("/");
  return cut > 0
    ? { connection: slug.slice(0, cut), model: slug.slice(cut + 1) }
    : null;
}

/** What the page has chosen for a role; `null` keeps the recorded selection. */
export type RolePick = Selection | null;

/** Triage rides the main client or has one of its own (docs/control-api.md). */
export type TriagePick =
  | { kind: "keep" }
  | { kind: "shared" }
  | { kind: "separate"; selection: Selection };

export interface SwitchDraft {
  main: RolePick;
  spotter: RolePick;
  triage: TriagePick;
}

/** Nothing chosen: every role keeps what the log records. */
export const keeping: SwitchDraft = {
  main: null,
  spotter: null,
  triage: { kind: "keep" },
};

/** A component as its row shows it: `connection/model`, the slug a reader
 * recognizes, unambiguous because the connection is named beside the id. */
export const selectionText = (selection: Selection | null): string | null =>
  selection === null ? null : `${selection.connection}/${selection.model}`;

/** What the triage row shows: the draft when it has moved, its source's own
 * answer when it shares main, and the recorded selection otherwise. */
export function triageValue(
  draft: SwitchDraft,
  recorded: RecordedRoles | null,
): string | null {
  if (draft.triage.kind === "shared") return "Same as main model";
  if (draft.triage.kind === "separate") {
    return selectionText(draft.triage.selection);
  }
  const current = recorded?.triage;
  if (current === undefined) return null;
  return current.shared ? "Same as main model" : selectionText(current);
}

/** Whether triage has a client of its own, either chosen or recorded. */
export function triageSeparate(
  draft: SwitchDraft,
  recorded: RecordedRoles | null,
): boolean {
  if (draft.triage.kind === "separate") return true;
  if (draft.triage.kind === "shared") return false;
  return recorded?.triage !== undefined && !recorded.triage.shared;
}

const same = (
  recorded: RecordedSelection | null | undefined,
  pick: RecordedSelection,
): boolean =>
  recorded != null &&
  recorded.connection === pick.connection &&
  recorded.model === pick.model;

/** The resume patch's switch fields (`src/inference/switch.rs`), or `null` when
 * the draft asks for nothing the session is not already doing. Only a role that
 * moves is named: a pick that repeats the recorded selection is not a restart,
 * and a resume would reload the session for no change. */
export function switchFields(
  draft: SwitchDraft,
  recorded: RecordedRoles | null,
): Record<string, unknown> | null {
  const fields: Record<string, unknown> = {};
  if (draft.main !== null && !same(recorded?.main, draft.main)) {
    fields.connection = draft.main.connection;
    fields.model = draft.main.model;
  }
  if (draft.spotter !== null && !same(recorded?.spotter, draft.spotter)) {
    fields.spotter_connection = draft.spotter.connection;
    fields.spotter_model = draft.spotter.model;
  }
  const triage = draft.triage;
  if (triage.kind === "shared") {
    if (recorded?.triage?.shared !== true) fields.triage = { mode: "shared" };
  } else if (triage.kind === "separate") {
    const current = recorded?.triage;
    if (!(
      current !== undefined &&
      current.shared === false &&
      same(current, triage.selection)
    )) {
      fields.triage = { mode: "separate", ...triage.selection };
    }
  }
  return Object.keys(fields).length === 0 ? null : fields;
}
