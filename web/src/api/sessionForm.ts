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

/** One connection's entry in the catalog (docs/model-catalog.md). `status` is
 * `fresh`, `stale`, `missing`, `failed`, or `unavailable` (no usable credential,
 * or disabled: nothing is read and `error` says why). `age` is seconds since the
 * stored copy was fetched. */
export interface CatalogEntry {
  connection: string;
  provider: string;
  status: "fresh" | "stale" | "missing" | "failed" | "unavailable";
  age?: number;
  error?: string;
  models: CatalogModel[];
}

/** One model row: `model` is the provider-native id a draft carries, and a
 * field the source did not declare is absent. */
export interface CatalogModel {
  model: string;
  group?: string;
  name?: string;
  /** Unix seconds. */
  created?: number;
  /** Tokens. */
  context?: number;
  price?: { input: number; output: number };
  reasoning?: boolean;
  images?: boolean;
  /** Rank of the matching recent pick, 1 first, counted across connections. */
  recent?: number;
}

/** Every configured connection's stored listing, most recent picks folded in.
 *
 * Never waits on a provider: a `missing` or `stale` connection starts a
 * background refresh and is served as stored, and a `failed` one keeps its old
 * copy. `cwd` (+ optional `config`) names the project scope whose recent picks
 * count; without one no row has a `recent`. */
export const fetchCatalog = (cwd: string, config: string | null) =>
  requestAt<{ connections: CatalogEntry[] }>(
    "GET",
    `/inference/catalog?cwd=${encodeURIComponent(cwd)}` +
      (config === null ? "" : `&config=${encodeURIComponent(config)}`),
  );

/** One connection's entry, refreshed and awaited when its stored copy is
 * missing or older than `max_age`. A failed read answers `200` with
 * `status: "failed"` and the old copy; an unknown connection is `404`. */
export const fetchConnection = (
  connection: string,
  cwd: string,
  config: string | null,
  maxAge?: number,
) =>
  requestAt<CatalogEntry>(
    "GET",
    `/inference/catalog/${encodeURIComponent(connection)}` +
      `?cwd=${encodeURIComponent(cwd)}` +
      (config === null ? "" : `&config=${encodeURIComponent(config)}`) +
      (maxAge === undefined ? "" : `&max_age=${maxAge}`),
  );

/** Create a session. `201` names the minted id; a refusal is `400` with the
 * reason and mints nothing, so a form that keeps its edits has nothing to
 * reconcile. The host establishes the environment from the user-wide hook (or
 * its own, when none is configured), so this client sends no `env` — that is
 * the whole of what creating from a browser changes. */
export const createSession = (body: unknown) =>
  requestAt<{ id: string }>("POST", "/sessions", body);
