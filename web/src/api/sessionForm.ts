// The new-session surface (docs/control-api.md): the remembered launch
// locations the form offers, the preview that resolves a proposed session
// without minting one, and the creation itself.
//
// All four are the *listener's* routes — there is no session-prefixed variant
// of any of them, so they are addressed at the root under either binding
// (api/base.ts), like the listing above.
//
// Preview and creation take one body (`sessionRequest` in ui/sessionForm.ts),
// because the host resolves both through the same flow: what preview reports is
// what creation builds. The client posts its own overrides to creation, never
// the effective values preview answered with — folding resolved values back in
// would turn every inherited default into an explicit override.

import { requestAt } from "./http.ts";

/** One remembered launch location. `cwd` and `config` are exactly what
 * `POST /sessions` takes back, so an entry replays a launch verbatim;
 * `config` null asks creation to probe, which is what a launch that named none
 * did. `config_key` is the identity the index stored it under and what
 * `DELETE /projects` takes — reported rather than recomputed, because
 * recomputing re-probes the filesystem. */
export interface ProjectEntry {
  label: string;
  cwd: string;
  config: string | null;
  config_key: string;
  /** Unix seconds. The index is most-recently-used first, which is the order
   * the form shows it in. */
  last_used_at: number;
}

/** The remembered locations, most recently used first. An entry whose
 * directory or named configuration is gone is left out of the answer rather
 * than deleted, so it returns when the volume does. */
export const fetchProjects = () =>
  requestAt<{ projects: ProjectEntry[] }>("GET", "/projects");

/** Forget one entry. `404` means it is already gone, which is the state the
 * caller asked for; the caller drops the row either way. Deletes the
 * remembered entry alone — no file, no directory, no session that ran there. */
export const forgetProject = (entry: ProjectEntry) =>
  requestAt<{ forgotten: { cwd: string; config_key: string } }>(
    "DELETE",
    `/projects?cwd=${encodeURIComponent(entry.cwd)}` +
      `&config_key=${encodeURIComponent(entry.config_key)}`,
  );

/** One effective field: what a session would actually get, and the precedence
 * branch that supplied it. `source: null` marks a field whose resolution
 * failed — the paired issue says why — while `value: null` with a source is an
 * ordinary omission. The two are different answers. */
export interface Effective<T> {
  value: T | null;
  source: SelectionSource | null;
}

/** Which branch supplied a value. Closed, because a client renders each
 * differently: an inherited value is editable in place, a requested one is the
 * edit. */
export type SelectionSource =
  | "request"
  | "history"
  | "user_default"
  | "main_provider"
  | "decision_connection"
  | "project_config"
  | "provider_default"
  | "shared_main";

export interface ModelSelection {
  connection: string;
  provider: string;
  model: string;
}

/** Triage resolves to sharing main or to a flattened separate selection, so an
 * inherited default previews as the separate client creation would build. */
export type EffectiveTriage =
  | { mode: "shared" }
  | ({ mode: "separate" } & ModelSelection & { effort?: string });

/** What a proposed session would resolve to. A preview, not a reservation: the
 * host retains nothing here and resolves again at creation. */
export interface SessionPreview {
  effective: {
    main: Effective<ModelSelection>;
    spotter: Effective<ModelSelection>;
    triage: Effective<EffectiveTriage>;
    effort: Effective<string>;
    temperature: Effective<number>;
  };
  issues: PreviewIssue[];
}

/** One field-level problem, resolved independently: a field with an issue has
 * no value *and* no source, and the other fields still answer. `field` names
 * the request field, so a client attaches the message to the control being
 * edited. */
export interface PreviewIssue {
  field: string;
  code: string;
  message: string;
}

/** Resolve a draft without minting a directory or preparing anything. Only a
 * malformed body is an error here — every field-level problem is an issue
 * inside a `200`. */
export const previewSession = (body: unknown) =>
  requestAt<SessionPreview>("QUERY", "/sessions", body);

/** Create a session. `201` names the minted id; a refusal is `400` with the
 * reason and mints nothing, so a form that keeps its edits has nothing to
 * reconcile. The host establishes the environment from the user-wide hook (or
 * its own, when none is configured), so this client sends no `env` — that is
 * the whole of what creating from a browser changes. */
export const createSession = (body: unknown) =>
  requestAt<{ id: string }>("POST", "/sessions", body);
