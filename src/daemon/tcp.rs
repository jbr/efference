//! The TCP bind: the web client's front door (docs/web-bind.md).
//!
//! An optional TCP listener beside the UDS — the same `/v1` router, with a hardening +
//! auth gate layered in front. The layering is structural, not inferred: this module's
//! handler stack is spawned only on the TCP listener, so the UDS never consults it and
//! a per-conn "which listener am I on?" guess (which would fail open) never exists.
//! Credentials are all derived from the machine root (`crate::auth`); the host-only
//! surfaces (`/auth/link`, `/auth/otpauth`, `/auth/bearer`) are mounted only on the
//! UDS spawn, because possession of the host machine is the enrollment authority.

use super::*;
use crate::auth::{self, BurnLedger, Lockout, Root};
use std::time::SystemTime;
use trillium::{KnownHeaderName, Method};
use trillium_cookies::{
    CookiesConnExt, CookiesHandler,
    cookie::{Cookie, SameSite},
};

/// What `efference serve --bind` resolved to — built by the CLI from flags, consumed
/// by [`arm`]. Deliberately not part of the session config: the config file is
/// model-proposable and fs-watched live, and network exposure must not be requestable
/// from inside the session (docs/web-bind.md).
pub struct TcpBind {
    /// The listen address (`host:port`). Loopback may be plaintext; anything else
    /// requires `tls` — the CLI enforces this before constructing one of these.
    pub addr: String,
    /// PEM cert chain + key (the `tailscale cert` output). `None` = plaintext loopback.
    pub tls: Option<TlsPem>,
    /// The public name browsers address the daemon by: the Host allowlist entry and
    /// the hostname in the printed bootstrap link.
    pub host: String,
    /// The machine root every credential derives from.
    pub root: Root,
    /// Launch the operator's browser at the bootstrap link once bound.
    pub open: bool,
}

/// Which credential a conn presented. The bind accepts two, and one route
/// ([`files`](super::files)) accepts only the browser's.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Credential {
    Cookie,
    Bearer,
}

/// A PEM cert chain and private key, read by the CLI so a bad path fails at flag
/// resolution rather than mid-boot.
pub struct TlsPem {
    pub cert: Vec<u8>,
    pub key: Vec<u8>,
}

/// The derived credential state the gate and the auth routes share. One per daemon,
/// built at boot from the injected root; holds no secret that can't be re-derived.
pub(crate) struct TcpAuth {
    root: Root,
    cookie_key: [u8; 32],
    totp_secret: [u8; 20],
    /// Host values (no port, lowercase) a request may address the daemon by — the
    /// DNS-rebinding check. The configured public name, plus the loopback spellings
    /// on a plaintext dev bind.
    hosts: Vec<String>,
    /// `scheme://host:port` for minted links.
    public_base: String,
    /// The TOTP rate limit (docs/web-bind.md): per-daemon, in-memory.
    lockout: Mutex<Lockout>,
    /// The shared record of accepted TOTP steps — user-scoped because every daemon
    /// under this user derives the same TOTP secret.
    ledger: BurnLedger,
}

/// The listener half of a [`TcpBind`], separated from the credential half by [`arm`]
/// so the UDS router can hold the [`TcpAuth`] before the TCP server spawns.
pub(crate) struct Listener {
    pub(crate) addr: String,
    pub(crate) tls: Option<TlsPem>,
    pub(crate) open: bool,
}

/// Split a bind into its credential state and its listener parameters.
pub(crate) fn arm(bind: TcpBind) -> (Arc<TcpAuth>, Listener) {
    let ledger = BurnLedger::user_scoped().unwrap_or_else(|| {
        // No home directory: fall back beside the cwd's session base. Weaker
        // (repo-scoped burns) but never silently absent.
        BurnLedger::new(std::path::Path::new(".efference").join("totp-burns"))
    });
    arm_with_ledger(bind, ledger)
}

/// [`arm`] with the burn ledger injected — the seam a test uses to keep burns out of
/// the real user-scoped file.
pub(crate) fn arm_with_ledger(bind: TcpBind, ledger: BurnLedger) -> (Arc<TcpAuth>, Listener) {
    let TcpBind {
        addr,
        tls,
        host,
        root,
        open,
    } = bind;
    let scheme = if tls.is_some() { "https" } else { "http" };
    let port = addr.rsplit(':').next().unwrap_or_default();
    let host = host.to_ascii_lowercase();
    let public_base = format!("{scheme}://{host}:{port}");
    let mut hosts = vec![host];
    if tls.is_none() {
        // A plaintext bind is loopback-only (CLI-enforced); accept its spellings.
        for loopback in ["localhost", "127.0.0.1", "[::1]"] {
            if !hosts.iter().any(|h| h == loopback) {
                hosts.push(loopback.to_string());
            }
        }
    }
    let auth = Arc::new(TcpAuth {
        cookie_key: root.cookie_key(),
        totp_secret: root.totp_secret(),
        root,
        hosts,
        public_base,
        lockout: Mutex::new(Lockout::default()),
        ledger,
    });
    (auth, Listener { addr, tls, open })
}

/// Register a [`Listener`]'s address on `config`: plaintext, or TLS paired with a QUIC
/// listener on the same port. One function because the host adopts the same listener
/// shape (docs/host.md) and a second copy of the pairing would drift from this one.
///
/// Generic over the server type only because `trillium-tokio` does not export the one
/// it uses; the bounds say what the body needs of it.
pub(crate) fn bind<S>(
    listener: &Listener,
    config: trillium_tokio::ListenerConfig<S>,
) -> io::Result<trillium_tokio::ListenerConfig<S>>
where
    S: trillium_server_common::Server,
    trillium_rustls::RustlsAcceptor: trillium_server_common::Acceptor<S::Transport>,
    trillium_quinn::QuicConfig: trillium_server_common::QuicConfig<S>,
{
    match &listener.tls {
        // TLS also serves HTTP/3: a QUIC listener on the same port (UDP), which the
        // builder auto-pairs with the TLS listener and advertises via `alt-svc:
        // h3` — browsers upgrade on their next request. The gate is
        // protocol-agnostic (the authority read handles h2/h3's pseudo-header).
        Some(pem) => config
            .bind_tls(
                listener.addr.as_str(),
                trillium_rustls::RustlsAcceptor::from_single_cert(&pem.cert, &pem.key),
            )?
            .bind_quic(
                listener.addr.as_str(),
                trillium_quinn::QuicConfig::from_single_cert(&pem.cert, &pem.key),
            ),
        None => config.bind_tcp(listener.addr.as_str()),
    }
}

/// Spawn the TCP listener with the gated handler stack, sharing the UDS server's
/// swansong (one shutdown domain) and its `Shared` state (same `/v1` surface).
/// `without_signals` because the UDS server already registered the signal handlers.
pub(crate) fn spawn(
    listener: Listener,
    auth: Arc<TcpAuth>,
    swansong: trillium_tokio::Swansong,
    shared: Arc<Shared>,
) -> io::Result<trillium_server_common::ServerHandle> {
    let handler = stack(auth);
    let config = trillium_tokio::config()
        .with_swansong(swansong)
        .with_shared_state(shared)
        .without_signals()
        .listeners();
    Ok(bind(&listener, config)?.spawn(handler))
}

impl TcpAuth {
    pub(crate) fn public_base(&self) -> &str {
        &self.public_base
    }

    /// The bootstrap link: the current TOTP code in the URL *fragment*, which never
    /// reaches server or proxy logs; burning on acceptance makes scrollback and
    /// browser-history residue inert (docs/web-bind.md).
    pub(crate) fn mint_link(&self) -> String {
        let code = auth::totp(&self.totp_secret, auth::timestep(SystemTime::now()));
        format!("{}/#token={}", self.public_base, auth::format_code(code))
    }

    /// The enrollment URI for a stock authenticator app. Host-only surface.
    pub(crate) fn otpauth_uri(&self) -> String {
        let host = self
            .hosts
            .first()
            .map(String::as_str)
            .unwrap_or("efference");
        auth::otpauth_uri(&self.totp_secret, host)
    }

    /// The program credential. Host-only surface.
    pub(crate) fn bearer(&self) -> String {
        self.root.bearer()
    }

    fn host_allowed(&self, host: &str) -> bool {
        let host = host_only(host).to_ascii_lowercase();
        self.hosts.contains(&host)
    }

    /// Which credential the conn presents, if any: the session cookie, or the
    /// bearer for programs (which can set headers where `EventSource` cannot).
    ///
    /// *Which* one matters, not just whether there is one — `/files` accepts only
    /// the cookie (docs/web-bind.md), so the kind has to survive this check rather
    /// than collapsing to a bool here.
    fn credential(&self, conn: &Conn) -> Option<Credential> {
        // Read through the jar `CookiesHandler` parsed, never `get_str` on the
        // raw header: the cookie header arrives as several field lines when an
        // h2/h3 client splits one line per cookie (RFC 9113 §8.2.3), and
        // because cookies ignore ports, any other service's cookie on this
        // host rides along and forces the split — `get_str` reads none of a
        // multi-valued header, so a browser sharing the host with any other
        // service fails auth that curl (one cookie, one line) passes.
        if let Some(value) = conn.cookies().get(auth::COOKIE_NAME).map(Cookie::value)
            && auth::cookie_is_valid(&self.cookie_key, value, SystemTime::now())
        {
            return Some(Credential::Cookie);
        }
        if let Some(authorization) = conn
            .request_headers()
            .get_str(KnownHeaderName::Authorization)
            && let Some(token) = authorization.strip_prefix("Bearer ")
            && self.root.bearer_matches(token.trim())
        {
            return Some(Credential::Bearer);
        }
        None
    }

    /// Verify a six-digit code under the lockout, burning the accepted step. The
    /// lockout lock is held across the (tiny) ledger file I/O so concurrent attempts
    /// serialize rather than sharing one failure count update.
    fn authenticate_code(&self, code: &str) -> Result<(), CodeRejection> {
        let mut lockout = self.lockout.lock().unwrap();
        let now = Instant::now();
        if let Some(remaining) = lockout.locked_for(now) {
            return Err(CodeRejection::Locked(remaining));
        }
        let Some(step) = auth::matching_timestep(&self.totp_secret, code, SystemTime::now()) else {
            lockout.failure(now);
            return Err(CodeRejection::Wrong);
        };
        match self.ledger.burn(step) {
            Ok(true) => {
                lockout.success();
                Ok(())
            }
            Ok(false) => {
                lockout.failure(now);
                Err(CodeRejection::Replayed)
            }
            // Fail closed: a code the ledger can't record is a code that can't be
            // proven single-use.
            Err(e) => {
                log::error!("burn ledger write failed: {e}");
                Err(CodeRejection::Ledger)
            }
        }
    }
}

/// Why a code was rejected — each maps to one response shape in the session route.
enum CodeRejection {
    /// Rate-limited; carries the remaining lockout.
    Locked(Duration),
    /// No timestep in the skew window produced this code.
    Wrong,
    /// The code verified but its step was already burned — a replay (or a stale
    /// scrollback link; the remedy is `efference open`).
    Replayed,
    /// The ledger could not record the burn; verification fails closed.
    Ledger,
}

/// Which surface a gated listener serves — the one thing that differs between the
/// two binds that share this gate. A session daemon serves the control API and the
/// operator's file view; the host serves session discovery and the per-session proxy
/// (docs/host.md). Everything else about the gate is identical, so the difference
/// lives here rather than in a second gate.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Surface {
    /// `efference serve --bind`: `/v1` takes either credential, `/files` the cookie
    /// only (docs/web-bind.md).
    Session,
    /// `efference host`: everything under `/sessions` takes either credential — the
    /// discovery listing and every proxied per-session path alike (docs/host.md).
    Host,
}

/// What a path costs on this surface.
enum Zone {
    /// No credential: the web client's shell and hashed assets, plus the mint.
    Anonymous,
    /// A cookie or a bearer.
    Credential,
    /// The cookie only; a valid bearer is refused with `403` rather than `401`.
    CookieOnly,
}

impl Surface {
    /// Whether `path` is one of the web client's own routes here — what the SPA
    /// index answers, and nothing else (docs/web-client.md).
    ///
    /// A session bind serves the client at its root, so its routes are exactly
    /// [`SPA_ROUTES`]. The host serves the same client at its root *and* the
    /// single-session view under `/sessions/<id>/`, which is the base that client
    /// joins its control paths onto (docs/host.md), so every route is answered
    /// there too. The enumeration stays exact either way: `/sessions/<id>/anything`
    /// that is not a named route is still a `404`.
    fn spa_route(self, path: &str) -> bool {
        SPA_ROUTES.contains(&path)
            || (matches!(self, Surface::Host)
                && session_scoped(path).is_some_and(|route| SPA_ROUTES.contains(&route)))
    }

    /// The guarded prefixes, and which credential each takes. A non-GET outside them
    /// fails closed at [`Zone::Credential`] rather than 404ing.
    fn zone(self, method: Method, path: &str) -> Zone {
        let under = |prefix: &str| path == prefix || path.starts_with(&format!("{prefix}/"));
        let guarded = match self {
            // `/files` is guarded on the same footing as `/v1`, and more narrowly: it
            // is the operator's read surface over the working directory, so it takes
            // the cookie only (docs/web-bind.md). A bearer holder reading it would
            // bypass `[paths]`, including the deny that keeps a session out of other
            // sessions' logs.
            Surface::Session if under("/files") => return Zone::CookieOnly,
            Surface::Session => under("/v1"),
            Surface::Host => under("/sessions"),
        };
        // The web client's shell is anonymous wherever it is served (docs/web-client.md).
        // On the host, the single-session view's own route sits *inside* `/sessions`,
        // so the exception is stated here rather than implied by the prefix: the
        // shell carries no session content, and the page's first act is to
        // authenticate.
        let guarded = guarded && !self.spa_route(path);
        match method {
            // `trillium_head::head()` runs ahead of the gate, so a HEAD arrives here
            // already rewritten to GET. Anonymous surface: every GET outside the
            // guarded prefixes — the web client's shell and hashed assets
            // (docs/web-client.md) — plus the credential mint.
            Method::Get if !guarded => Zone::Anonymous,
            Method::Post if path == "/auth/session" => Zone::Anonymous,
            _ => Zone::Credential,
        }
    }
}

/// The hardening + auth gate, run before every route on a TCP listener
/// (docs/web-bind.md): Host allowlist (DNS rebinding), Origin + Content-Type checks on
/// unsafe methods (CSRF — a localhost or LAN bind is not a trust boundary while a
/// browser is present), then the credential check against the listener's [`Surface`].
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
                // names a host we don't answer for. Programs (bearer) send none.
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
                // Form posts can only spell form encodings, so requiring JSON on
                // body-bearing methods closes the no-preflight CSRF shapes.
                if matches!(method, Method::Post | Method::Put | Method::Patch) {
                    let json = conn
                        .request_headers()
                        .get_str(KnownHeaderName::ContentType)
                        .is_some_and(|ct| ct.trim_start().starts_with("application/json"));
                    if !json {
                        return fail(
                            conn,
                            ErrorCode::BadRequest,
                            "unsafe methods require content-type: application/json",
                        );
                    }
                }
            }
            let zone = surface.zone(method, conn.path());
            if matches!(zone, Zone::Anonymous) {
                return conn;
            }
            match (auth.credential(&conn), zone) {
                (None, _) => fail(
                    conn,
                    ErrorCode::Unauthorized,
                    "no valid session cookie or bearer; POST /auth/session with a current code",
                ),
                // Not `Unauthorized`: the credential is valid, and re-authenticating
                // as the same program would not help. This surface is the browser's.
                (Some(Credential::Bearer), Zone::CookieOnly) => fail(
                    conn,
                    ErrorCode::Forbidden,
                    "/files takes the session cookie, not the bearer",
                ),
                (Some(_), _) => conn,
            }
        }
    }
}

/// The handler stack the TCP listener mounts, in one place so the tests exercise
/// the same one rather than a copy that can drift from it.
pub(crate) fn stack(auth: Arc<TcpAuth>) -> impl trillium::Handler {
    stack_with_files(auth, super::files::Files::at_cwd())
}

/// [`stack`] with the file root injected, so tests can serve a scratch directory
/// instead of whatever the test process's cwd happens to be.
pub(crate) fn stack_with_files(
    auth: Arc<TcpAuth>,
    files: super::files::Files,
) -> impl trillium::Handler {
    (
        // Outermost, so its `before_send` runs last and the headers land on every
        // response the stack produces — including the routers' errors.
        SecurityHeaders,
        // HEAD support: rewrites to GET on the way in, strips the body (keeping
        // content-length) on the way out. Before the gate, so it only ever sees GET.
        trillium_head::head(),
        // Parses the request cookie jar (every field line — see `authed`) and
        // serializes the response's. Ahead of the gate, which reads the jar.
        CookiesHandler::new(),
        gate(Arc::clone(&auth), Surface::Session),
        public_router(auth),
        router(),
        // The operator's read surface over the working directory (docs/web-bind.md),
        // mounted only here so the UDS never serves it. It claims `/files` and
        // passes everything else through.
        files,
        // Last: any GET neither router nor `/files` claimed is the web client's surface
        // (docs/web-client.md) — an asset, or the SPA index.
        frontend(Surface::Session),
    )
}

/// The policy the embedded build is served under. Everything the page loads is
/// same-origin and external — one module script and one stylesheet, both hashed,
/// with the grammar chunks imported dynamically from the same origin — so `'self'`
/// needs no hash, nonce, or `unsafe-` escape. Syntax highlighting needs no
/// `style-src` accommodation: token colours are set through React's `style` prop,
/// which is CSSOM, and CSP governs inline `<style>` elements and `style=""`
/// attributes parsed from markup rather than CSSOM mutations
/// (docs/web-client.md).
#[cfg(not(feature = "dev-proxy"))]
const CSP: &str = "default-src 'self'; base-uri 'none'; frame-ancestors 'none'; \
                   form-action 'none'; object-src 'none'";

/// Under `dev-proxy` the page is Vite's, not the build's: it injects an inline
/// module preamble and talks to its HMR server over a WebSocket. Relaxing for that
/// means the strict policy above is not the one exercised in development — the
/// reason this is gated on the same `cfg` as the proxy rather than on a runtime
/// flag, so a release build cannot serve it.
#[cfg(feature = "dev-proxy")]
const CSP: &str = "default-src 'self'; script-src 'self' 'unsafe-inline' 'unsafe-eval'; \
                   style-src 'self' 'unsafe-inline'; connect-src 'self' ws: wss:; \
                   base-uri 'none'; frame-ancestors 'none'; object-src 'none'";

/// Response headers that bound what a string escaping the client's escaping could
/// do. Defence in depth behind the invariant that nothing turns untrusted content
/// into markup (docs/web-client.md): every body on this page is model-
/// or tool-authored, and the page holds a credential for a channel that runs
/// commands. `frame-ancestors 'none'` also keeps the session out of an iframe,
/// which no surface here wants.
pub(crate) struct SecurityHeaders;

impl trillium::Handler for SecurityHeaders {
    async fn before_send(&self, conn: trillium::Conn) -> trillium::Conn {
        conn.with_response_header(KnownHeaderName::ContentSecurityPolicy, CSP)
            .with_response_header("x-content-type-options", "nosniff")
            // Redundant with `frame-ancestors` for anything current, and one
            // header for the browsers that predate CSP3.
            .with_response_header("x-frame-options", "DENY")
            .with_response_header(KnownHeaderName::ReferrerPolicy, "no-referrer")
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
/// - `/tree` — the file tree and finder, reachable without a call having named
///   a file (docs/web-client.md "Navigating the working directory").
const SPA_ROUTES: [&str; 2] = ["/", "/tree"];

/// [`Surface::spa_route`] as the fn pointer [`frontend`] takes, one per surface.
fn session_spa_route(conn: &Conn) -> bool {
    Surface::Session.spa_route(conn.path())
}

fn host_spa_route(conn: &Conn) -> bool {
    Surface::Host.spa_route(conn.path())
}

/// The client route inside a `/sessions/<id>` prefix, if the path carries one:
/// `/sessions/work/tree` → `/tree`, and both `/sessions/work` and `/sessions/work/`
/// → `/`. `None` when the path is not session-prefixed, or when the id is not a
/// single plain segment — the same rule the host resolves ids by, so the shell is
/// offered for exactly the ids the proxy would accept.
fn session_scoped(path: &str) -> Option<&str> {
    let after = path.strip_prefix("/sessions/")?;
    let (id, rest) = after.split_at(after.find('/').unwrap_or(after.len()));
    crate::host::plain_id(id).then_some(if rest.is_empty() { "/" } else { rest })
}

/// The web client (docs/web-client.md): `web/` built and embedded at compile time,
/// or — under the `dev-proxy` feature — proxied to a spawned Vite dev server with
/// HMR. Mounted after both routers on the TCP spawn, so it answers exactly the GETs
/// they don't claim; the gate has already let those through anonymously. The host
/// mounts the same build at its own root (docs/host.md), which is why the index
/// predicate is surface-dependent: the host also answers the client's routes under
/// a session prefix, and the session bind does not.
pub(crate) fn frontend(surface: Surface) -> impl trillium::Handler {
    trillium_frontend::frontend!("./web")
        .with_client(trillium_client::Client::new(
            trillium_tokio::ClientConfig::default(),
        ))
        .with_index_file("index.html")
        // `trillium_frontend` takes a bare fn pointer, so the surface is chosen
        // between two fn items rather than captured in a closure.
        .with_index_predicate(match surface {
            Surface::Session => session_spa_route,
            Surface::Host => host_spa_route,
        })
}

/// The browser-facing anonymous surface, mounted only on the TCP spawn: the
/// credential mint. Static assets fall through to [`frontend`]; everything else on
/// TCP is the shared `/v1` router behind the gate.
pub(crate) fn public_router(auth: Arc<TcpAuth>) -> Router {
    Router::new()
        // Trade a current TOTP code for the session cookie. The one credential mint;
        // rate-limited, and every accepted code burns. The login view that reads
        // `#token=` from the fragment and POSTs here is the web client's
        // (web/src/ui/Login.tsx).
        .post("/auth/session", move |mut conn: Conn| {
            let auth = Arc::clone(&auth);
            async move {
                #[derive(Deserialize)]
                struct SessionBody {
                    code: String,
                }
                let body = conn.request_body_string().await.unwrap_or_default();
                let code = match serde_json::from_str::<SessionBody>(&body) {
                    Ok(body) => body.code,
                    Err(e) => {
                        return fail(
                            conn,
                            ErrorCode::BadRequest,
                            format!("bad /auth/session body: {e}"),
                        );
                    }
                };
                match auth.authenticate_code(code.trim()) {
                    Ok(()) => {
                        let value = auth::mint_cookie(&auth.cookie_key, SystemTime::now());
                        let cookie = Cookie::build((auth::COOKIE_NAME, value))
                            .path("/")
                            .secure(true)
                            .http_only(true)
                            .same_site(SameSite::Strict)
                            .max_age(auth::COOKIE_TTL.try_into().expect("30 days fits"))
                            .build();
                        conn.with_cookie(cookie)
                            .with_status(200)
                            .with_body(r#"{"status":"authed"}"#)
                            .halt()
                    }
                    Err(CodeRejection::Locked(remaining)) => {
                        let secs = remaining.as_secs().max(1);
                        respond(
                            conn.with_response_header(
                                KnownHeaderName::RetryAfter,
                                secs.to_string(),
                            ),
                            ErrorBody::new(
                                ErrorCode::RateLimited,
                                format!("verification locked; retry in {secs}s"),
                            ),
                        )
                    }
                    Err(CodeRejection::Wrong) => {
                        fail(conn, ErrorCode::Unauthorized, "code did not verify")
                    }
                    Err(CodeRejection::Replayed) => fail(
                        conn,
                        ErrorCode::Unauthorized,
                        "code already used — mint a fresh link with `efference open`",
                    ),
                    Err(CodeRejection::Ledger) => {
                        fail(conn, ErrorCode::Internal, "could not record the code burn")
                    }
                }
            }
        })
}

/// The host-only credential surface, mounted only on the UDS spawn — possession of
/// the host machine is the enrollment authority (docs/web-bind.md). The `otpauth`
/// URI in particular carries the TOTP secret itself: a strictly stronger credential
/// than a cookie, so it must never ride the TCP listener even authenticated.
/// With no bind armed the routes exist but answer 404, so the CLI verb gets a
/// sentence instead of a route mismatch.
pub(crate) fn host_router(auth: Option<Arc<TcpAuth>>) -> Router {
    let for_link = auth.clone();
    let for_otpauth = auth.clone();
    let for_bearer = auth;
    Router::new()
        .get("/auth/link", move |conn: Conn| {
            let auth = for_link.clone();
            async move {
                match auth {
                    Some(auth) => conn
                        .with_status(200)
                        .with_body(serde_json::json!({ "url": auth.mint_link() }).to_string())
                        .halt(),
                    None => no_bind(conn),
                }
            }
        })
        .get("/auth/otpauth", move |conn: Conn| {
            let auth = for_otpauth.clone();
            async move {
                match auth {
                    Some(auth) => conn
                        .with_status(200)
                        .with_body(serde_json::json!({ "uri": auth.otpauth_uri() }).to_string())
                        .halt(),
                    None => no_bind(conn),
                }
            }
        })
        .get("/auth/bearer", move |conn: Conn| {
            let auth = for_bearer.clone();
            async move {
                match auth {
                    Some(auth) => conn
                        .with_status(200)
                        .with_body(serde_json::json!({ "bearer": auth.bearer() }).to_string())
                        .halt(),
                    None => no_bind(conn),
                }
            }
        })
}

fn no_bind(conn: Conn) -> Conn {
    fail(
        conn,
        ErrorCode::NotFound,
        "no TCP bind armed — start serve with --bind",
    )
}

/// The host part of an authority-shaped value: strip a port and a trailing FQDN dot,
/// keep IPv6 brackets whole.
fn host_only(value: &str) -> &str {
    if value.starts_with('[') {
        match value.find(']') {
            Some(end) => &value[..=end],
            None => value,
        }
    } else {
        let host = value.split(':').next().unwrap_or(value);
        host.strip_suffix('.').unwrap_or(host)
    }
}

/// The host part of an `Origin` header (`scheme://host[:port]`).
fn origin_host(origin: &str) -> &str {
    let rest = origin.split_once("://").map_or(origin, |(_, rest)| rest);
    host_only(rest)
}

#[cfg(test)]
mod test;
