//! The hardening + auth gate and the anonymous surface it prices (docs/web-bind.md).
//!
//! [`Surface::anonymous`] names what costs nothing and everything else costs a device token;
//! [`SPA_ROUTES`], [`ASSET_PREFIXES`], and [`SHELL_ASSETS`] name the web client's shell,
//! which loads before anything can authenticate (docs/web-client.md).

use super::*;
use trillium::{KnownHeaderName, Method};

/// Which surface a gated listener serves — the one thing that differs between the
/// two binds that share this gate. A session daemon serves the control API and the
/// operator's file view; a host serves the session listing and every session's
/// control API under a `/sessions/<id>` prefix (docs/web-bind.md). Everything else
/// about the gate is identical, so the difference lives here rather than in a
/// second gate.
///
/// A surface is not a set of guarded prefixes: [`Surface::anonymous`] names what
/// costs nothing and everything else costs a device token, so what a surface settles
/// is where the client's own routes sit (at the root on a session bind, under
/// `/sessions/<id>` as well on a host) and the binding probe reaching a session
/// bind's router.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Surface {
    /// A session bind: `/v1` and `/files` take a device token (docs/web-bind.md).
    Session,
    /// A multi-session host: everything under `/sessions` takes a device token — the
    /// session listing and every session-prefixed path alike, `/files` among them
    /// (docs/web-bind.md).
    Host,
}

impl Surface {
    /// Whether `path` is one of the web client's own routes here — what the SPA
    /// index answers, and nothing else (docs/web-client.md).
    ///
    /// A session bind serves the client at its root, so its routes are exactly
    /// [`SPA_ROUTES`]. A host serves the same client at its root *and* the
    /// single-session view under `/sessions/<id>/`, which is the base that client
    /// joins its control paths onto (docs/web-client.md), so every route is answered
    /// there too. The enumeration stays exact either way: `/sessions/<id>/anything`
    /// that is not a named route is still a `404`.
    pub(in crate::daemon) fn spa_route(self, path: &str) -> bool {
        SPA_ROUTES.contains(&path)
            || (matches!(self, Surface::Host)
                && session_scoped(path).is_some_and(|route| SPA_ROUTES.contains(&route)))
    }

    /// Whether `path` costs nothing here; everything else costs a device token.
    ///
    /// The rule is the anonymous list inverted: a route added later is guarded
    /// unless it is named here too. Stating it the other way — a list of guarded
    /// prefixes — fails the other way, leaving every new top-level route readable
    /// to anyone who can reach the bind. [`trillium_head::head`] runs ahead of the
    /// gate, so a HEAD arrives here already rewritten to GET.
    fn anonymous(self, method: Method, path: &str) -> bool {
        let get = method == Method::Get;
        // The web client's own shell, wherever it is served: its routes at its
        // base, the build's hashed assets, the launch images under `/splash`, and
        // the few public files whose names the build does not hash. None of it is
        // session content, and the sign-in view lives inside it (docs/web-client.md).
        // The shell lists mirror the service worker's precache (`web/public/sw.js`),
        // which has to agree: a file in one and not the other is a shell that cannot
        // load itself. `/splash` is the exception: iOS reads the launch images only
        // when the app is added to the home screen, so the worker does not cache them.
        if get
            && (self.spa_route(path)
                || ASSET_PREFIXES.iter().any(|prefix| under(path, prefix))
                || SHELL_ASSETS.contains(&path))
        {
            return true;
        }
        // A browser asking to sign in, and collecting its token once the host
        // approves: the sign-in cannot present the credential it is there to obtain.
        // Neither authorizes anything a peer can guess — the claim is 128 random bits,
        // and approval is the socket's (docs/web-bind.md).
        if method == Method::Post && matches!(path, "/pair" | "/pair/claim") {
            return true;
        }
        // The dev server's module graph, in the build that proxies one. The
        // shell is loaded before anything can authenticate, so these are the
        // requests a credential would lock the sign-in view out behind.
        #[cfg(feature = "dev-proxy")]
        if get
            && (path.starts_with(VITE_PREFIX)
                || DEV_PROXY_DIRS.iter().any(|prefix| under(path, prefix)))
        {
            return true;
        }
        // The web client tells the two bindings apart by probing `GET /sessions`
        // (`web/src/session/entry.ts`): a host answers the listing, a session
        // daemon serves no such route. It reads that `404` as "not a host", so
        // the probe has to reach the router — a credential check ahead of it
        // would answer `401`, which is how a host with no credential yet reads,
        // and a session daemon's root would read as a host awaiting sign-in.
        matches!(self, Surface::Session) && get && path == "/sessions"
    }
}

/// Whether `path` is `prefix` itself or under it.
pub(super) fn under(path: &str, prefix: &str) -> bool {
    path == prefix || path.starts_with(&format!("{prefix}/"))
}

/// The hardening + auth gate, run before every route on a TCP listener
/// (docs/web-bind.md): Host allowlist (DNS rebinding), Origin + Content-Type checks on
/// unsafe methods (CSRF — a localhost or LAN bind is not a trust boundary while a
/// browser is present), then the credential check against the listener's [`Surface`]:
/// a path it does not name as free takes a device token whose device is enrolled.
pub(crate) fn gate(auth: Arc<TcpAuth>, surface: Surface) -> impl trillium::Handler {
    move |conn: Conn| {
        let auth = Arc::clone(&auth);
        async move {
            // h1 carries the authority in the Host header; h2 (which the TLS
            // acceptor's ALPN negotiates with every browser) carries it in the
            // `:authority` pseudo-header, which `Conn::host()` does not consult
            // (trillium-http 1.6) — read both off the inner conn.
            let inner: &trillium_http::Conn<_> = conn.as_ref();
            let Some(host) = inner
                .host()
                .or_else(|| inner.authority())
                .map(str::to_string)
            else {
                return fail(conn, ErrorCode::Forbidden, "request names no host");
            };
            if !auth.host_allowed(&host) {
                return fail(
                    conn,
                    ErrorCode::Forbidden,
                    format!("host {host:?} is not this daemon's name"),
                );
            }
            let method = conn.method();
            let safe = matches!(method, Method::Get | Method::Head | Method::Options);
            if !safe {
                // A browser attaches `Origin` to unsafe requests; a cross-site one
                // names a host we don't answer for. Programs send none.
                let origin = conn
                    .request_headers()
                    .get_str(KnownHeaderName::Origin)
                    .map(str::to_string);
                if let Some(origin) = origin
                    && !auth.host_allowed(origin_host(&origin))
                {
                    return fail(
                        conn,
                        ErrorCode::Forbidden,
                        format!("origin {origin:?} is not this daemon"),
                    );
                }
                // Form posts can only spell form encodings, and a cross-site `fetch`
                // with any other type is preflighted (which this listener never
                // answers), so requiring one of these two on body-bearing methods
                // closes the no-preflight CSRF shapes. Raw bytes are the upload
                // route's (`POST /attachments`, docs/inbox.md).
                if matches!(method, Method::Post | Method::Put | Method::Patch) {
                    let preflighted = conn
                        .request_headers()
                        .get_str(KnownHeaderName::ContentType)
                        .is_some_and(|ct| {
                            let ct = ct.trim_start();
                            ct.starts_with("application/json")
                                || ct.starts_with("application/octet-stream")
                        });
                    if !preflighted {
                        return fail(
                            conn,
                            ErrorCode::BadRequest,
                            "unsafe methods require content-type: application/json \
                             (or application/octet-stream for an upload)",
                        );
                    }
                }
            }
            // Checked first, so a free path never reaches the device table.
            if surface.anonymous(method, conn.path()) {
                return conn;
            }
            match auth.credential(&conn) {
                Credential::Enrolled => conn,
                Credential::Unchecked => fail(
                    conn,
                    ErrorCode::Internal,
                    "could not read the enrolled devices, so the token was not checked",
                ),
                Credential::Refused => fail(
                    conn,
                    ErrorCode::Unauthorized,
                    "no valid `Authorization: Bearer` device token; a browser gets one by \
                     signing in with a code `efference devices approve` takes at the host, \
                     and `efference devices add` there mints one for a script",
                ),
            }
        }
    }
}

/// Every path the SPA index answers, **relative to the client's base** — the whole
/// of the client's routing, in one place. The base is empty on a session bind and
/// `/sessions/<id>` on the host, so each route is also a client route under that
/// prefix; [`Surface::spa_route`] is where the two spellings meet.
///
/// Anything not named here 404s, deliberately: answering every unmatched
/// path with the index would hand scanner probes a `200`, and this bind is
/// reachable from a tailnet.
///
/// The list is a list rather than a prefix rule because two things must agree on
/// it: this predicate, and the service worker's navigation fallback
/// (`web/public/sw.js`), which serves the cached shell for exactly these paths
/// when offline. A route added to one and not the other is a route that works
/// online and 404s from the home screen, or the reverse. New client routing is
/// added here, not by loosening the predicate.
///
/// - `/` — the session view, and the installed app's `start_url`. At a host's root
///   it is the session list instead; the client tells the two bindings apart by
///   probing `GET /sessions` (docs/web-client.md).
/// - `/tree` — the file tree and its path filter, reachable without a call having named
///   a file (docs/web-client.md "Navigating the working directory").
const SPA_ROUTES: [&str; 2] = ["/", "/tree"];

/// The build's own directories, served at the listener's root whichever binding
/// answered the shell. `/assets` and `/fonts` mirror `isCacheable` in
/// `web/public/sw.js`. `/splash` holds the launch images (docs/web-client.md
/// "Installability"): iOS reads them only at add-to-home time, so the worker
/// does not cache them.
const ASSET_PREFIXES: [&str; 3] = ["/assets", "/fonts", "/splash"];

/// Public files the build does not hash, so they are named rather than found by
/// prefix. Mirrors `SHELL_ASSETS` in `web/public/sw.js`.
const SHELL_ASSETS: [&str; 8] = [
    "/manifest.webmanifest",
    "/icon.svg",
    "/icon-192.png",
    "/icon-512.png",
    "/icon-maskable-512.png",
    "/apple-touch-icon.png",
    "/paper.svg",
    // The worker script itself, which the browser fetches to register the
    // worker and which nothing precaches (`web/src/pwa.ts`). It is fetched on a
    // bare visit, before any credential, so a gate that refused it would cost
    // installability and the offline shell on exactly the first load.
    "/sw.js",
];

/// Vite's own URLs, served only by the `dev-proxy` build, which proxies the
/// whole module graph through the gate. Prefixes rather than files because the
/// graph is generated. A page that cannot load these cannot render the sign-in
/// view, so there would be nothing left to ask for a credential with.
///
/// Vite's virtual modules — `/@vite/client`, `/@react-refresh`, `/@fs/…` — share
/// a leading `/@` with no separator after it, so it is a plain string prefix
/// rather than a directory: [`under`] would require `/@/` and match none of them.
#[cfg(feature = "dev-proxy")]
const VITE_PREFIX: &str = "/@";

/// The dev server's directories: the app's own sources and the pre-bundled
/// dependencies, matched as directories by [`under`].
#[cfg(feature = "dev-proxy")]
const DEV_PROXY_DIRS: [&str; 2] = ["/src", "/node_modules"];

/// The client route inside a `/sessions/<id>` prefix, if the path carries one:
/// `/sessions/work/tree` → `/tree`, and both `/sessions/work` and `/sessions/work/`
/// → `/`. `None` when the path is not session-prefixed, or when the id is not a
/// single plain segment — the same rule a host resolves ids by, so the shell is
/// offered for exactly the ids a host would accept.
pub(super) fn session_scoped(path: &str) -> Option<&str> {
    let after = path.strip_prefix("/sessions/")?;
    let (id, rest) = after.split_at(after.find('/').unwrap_or(after.len()));
    plain_id(id).then_some(if rest.is_empty() { "/" } else { rest })
}

/// Whether `id` is a single plain path segment — ASCII alphanumerics, `-`, `_`, `.`,
/// and neither `.` nor `..`. Anything else names no session: an id arrives from a
/// URL and is joined onto a filesystem path, so it is validated before the join
/// rather than trusted. The web client's base derivation (`web/src/api/base.ts`)
/// accepts the same set.
pub(crate) fn plain_id(id: &str) -> bool {
    !id.is_empty()
        && id != "."
        && id != ".."
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
}

#[cfg(test)]
mod test;
