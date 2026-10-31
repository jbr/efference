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
import type {
  ConnectionCatalog,
  CatalogList,
  CreatedSession,
  ForgottenProject,
  ModelRole,
  ProjectEntry,
  ProjectList,
  ResolvedOptions,
} from "./types.ts";

/** The remembered locations, most recently used first. An entry whose
 * directory or named configuration is gone is left out of the answer rather
 * than deleted, so it returns when the volume does. */
export const fetchProjects = () => requestAt<ProjectList>("GET", "/projects");

/** Forget one entry. `404` means it is already gone, which is the state the
 * caller asked for; the caller drops the row either way. Deletes the
 * remembered entry alone — no file, no directory, no session that ran there. */
export const forgetProject = (entry: ProjectEntry) =>
  requestAt<ForgottenProject>(
    "DELETE",
    `/projects?cwd=${encodeURIComponent(entry.cwd)}` +
      `&config_key=${encodeURIComponent(entry.config_key)}`,
  );

/** Resolve a draft without minting a directory or preparing anything. Only a
 * malformed body is an error here — every field-level problem is an issue
 * inside a `200`. */
export const previewSession = (body: unknown) =>
  requestAt<ResolvedOptions>("QUERY", "/sessions", body);

/** Every configured connection's stored listing, most recent picks folded in.
 *
 * Never waits on a provider: a `missing` or `stale` connection starts a
 * background refresh and is served as stored, and a `failed` one keeps its old
 * copy. `cwd` (+ optional `config`) names the project scope whose recent picks
 * count; without one no row has a `recent`. `role` narrows the answer to what
 * that role may choose: no decision models, and no connection that serves none,
 * for `main` or `triage`; everything for `spotter`. */
export const fetchCatalog = (
  cwd: string,
  config: string | null,
  role: ModelRole,
) =>
  requestAt<CatalogList>(
    "GET",
    `/inference/catalog?cwd=${encodeURIComponent(cwd)}` +
      (config === null ? "" : `&config=${encodeURIComponent(config)}`) +
      `&role=${role}`,
  );

/** One connection's entry, refreshed and awaited when its stored copy is
 * missing or older than `max_age`. A failed read answers `200` with
 * `status: "failed"` and the old copy; an unknown connection is `404`. */
export const fetchConnection = (
  connection: string,
  cwd: string,
  config: string | null,
  role: ModelRole,
  maxAge?: number,
) =>
  requestAt<ConnectionCatalog>(
    "GET",
    `/inference/catalog/${encodeURIComponent(connection)}` +
      `?cwd=${encodeURIComponent(cwd)}` +
      (config === null ? "" : `&config=${encodeURIComponent(config)}`) +
      `&role=${role}` +
      (maxAge === undefined ? "" : `&max_age=${maxAge}`),
  );

/** Create a session. `201` names the minted id; a refusal is `400` with the
 * reason and mints nothing, so a form that keeps its edits has nothing to
 * reconcile. The host establishes the environment from the user-wide hook (or
 * its own, when none is configured), so this client sends no `env` — that is
 * the whole of what creating from a browser changes. */
export const createSession = (body: unknown) =>
  requestAt<CreatedSession>("POST", "/sessions", body);
