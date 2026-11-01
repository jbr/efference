// Where this client's control endpoint is (docs/control-api.md): a base URL the
// api layer joins every path onto. A session daemon serves the client at its
// socket root, a host serves it under `/sessions/<id>/` and routes that
// prefix to the session (docs/web-client.md) — so the two bindings are the
// same client code with a different base, and nothing above this module names a
// binding.
//
// The base is read off the page's own location rather than configured: the page
// was served *from* its base, so the path it loaded at is the answer. Derived
// per call rather than cached at module load, so nothing depends on when this
// module is first imported.

/** A session id as the host resolves one: a single plain segment — ASCII
 * alphanumerics, `-`, `_`, `.`, and neither `.` nor `..`. Mirrors `plain_id`
 * (src/record/session_dir.rs); an id outside it names no session there, so a path
 * carrying one is not a session prefix here either. */
const plainId = (id: string): boolean =>
  id.length > 0 && id !== "." && id !== ".." && /^[A-Za-z0-9._-]+$/.test(id);

/**
 * The control base for a page served at `pathname`: `/sessions/<id>` when it is
 * served under a session prefix, `""` otherwise. Pure, so the rule is testable
 * without a location.
 */
export function deriveBase(pathname: string): string {
  const after = pathname.startsWith("/sessions/")
    ? pathname.slice("/sessions/".length)
    : null;
  if (after === null) return "";
  const end = after.indexOf("/");
  const id = end === -1 ? after : after.slice(0, end);
  return plainId(id) ? `/sessions/${id}` : "";
}

/**
 * The client route the page is on, with any base removed — one of the paths the
 * server's SPA index answers (`SPA_ROUTES`, src/daemon/tcp/gate.rs). `/sessions/x/tree`
 * and `/tree` are the same route in two bindings.
 */
export function deriveRoute(pathname: string): string {
  const rest = pathname.slice(deriveBase(pathname).length);
  return rest === "" ? "/" : rest;
}

const pathname = (): string =>
  typeof location === "undefined" ? "/" : location.pathname;

/** The base every control path joins onto. `""` at a session daemon's root. */
export const apiBase = (): string => deriveBase(pathname());

/** The session id the page is showing, or `null` when the base is the root — in
 * which case *which* session is a question only the endpoint answers. */
export const sessionId = (): string | null =>
  apiBase() === "" ? null : apiBase().slice("/sessions/".length);

/** The client route this page loaded at. */
export const clientRoute = (): string => deriveRoute(pathname());

/** Join a control path onto the base. Every `/v1/…` and `/files/…` URL the
 * client builds goes through here; `/auth/…` and `/sessions` do not — those are
 * the *listener's* surfaces, at its root under either binding. An explicit base
 * binds work that outlives navigation to its originating session. */
export const join = (path: string, base = apiBase()): string => base + path;

/** The link into one session's view. Client-side navigation remounts the view
 * against its control endpoint (docs/web-client.md). */
export const sessionHref = (id: string): string => `/sessions/${id}/`;
