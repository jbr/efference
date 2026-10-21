//! The TCP bind: the web client's front door (docs/web-bind.md).
//!
//! An optional TCP listener beside the UDS — the same `/v1` routes, with a hardening +
//! auth gate layered in front. This module holds the listener, the credential state,
//! the gate, the mint and enrollment routes, and the web client handler; `handler.rs`
//! is the one place they are assembled into a stack, keyed on the transport, so the
//! UDS never consults the gate and a per-conn "which listener am I on?" guess (which
//! would fail open) never exists. Credentials are all derived from the machine root
//! (`crate::auth`); the enrollment surfaces (`/auth/link`, `/auth/otpauth`,
//! `/auth/bearer`) are mounted only on the socket, because possession of the host
//! machine is the enrollment authority.

use super::*;
use crate::auth::{self, BurnLedger, Lockout, Root};
use std::time::SystemTime;
use trillium::{KnownHeaderName, Method};
use trillium_cookies::{
    CookiesConnExt,
    cookie::{Cookie, SameSite},
};

/// What `--bind` resolved to — built by the CLI from flags, consumed
/// by [`arm`]. Deliberately not part of the session config: the config file is
/// model-proposable and fs-watched live, and network exposure must not be requestable
/// from inside the session (docs/web-bind.md).
pub struct TcpBind {
    /// The listen address (`host:port`). Loopback may be plaintext; anything else
    /// requires `tls` — the CLI enforces this before constructing one of these.
    pub addr: String,
    /// PEM cert chain + key (the `tailscale cert` output). `None` = plaintext loopback.
    pub tls: Option<TlsPem>,
    /// The address browsers open, `scheme://name[:port]` with no path: the base of
    /// the printed bootstrap link, and — by its host — the Host allowlist entry. It
    /// differs from the listener when something in front terminates TLS (a
    /// `tailscale serve`), which is why it is given rather than derived.
    pub public_url: String,
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
pub struct TcpAuth {
    root: Root,
    cookie_key: [u8; 32],
    totp_secret: [u8; 20],
    /// Host values (no port, lowercase) a request may address the daemon by — the
    /// DNS-rebinding check. The configured public name, plus the loopback spellings
    /// on a plaintext dev bind.
    hosts: Vec<String>,
    /// `scheme://host:port` for minted links. Behind a lock because a bind to
    /// port 0 learns its port only once listening ([`TcpAuth::bound_port`]).
    public_base: Mutex<String>,
    /// Plaintext loopback: the session cookie is [`auth::LOOPBACK_COOKIE_NAME`]
    /// without `Secure`, which Safari refuses over http.
    plaintext: bool,
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
    let ledger = BurnLedger::user_scoped();
    arm_with_ledger(bind, ledger)
}

/// [`arm`] with the burn ledger injected — the interface a test uses to keep burns out of
/// the real user-scoped file.
pub(crate) fn arm_with_ledger(bind: TcpBind, ledger: BurnLedger) -> (Arc<TcpAuth>, Listener) {
    let TcpBind {
        addr,
        tls,
        public_url,
        root,
        open,
    } = bind;
    let public_base = public_url.trim_end_matches('/').to_string();
    let authority = public_base
        .split_once("://")
        .map_or(public_base.as_str(), |(_, rest)| rest);
    let mut hosts = vec![host_only(authority).to_ascii_lowercase()];
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
        public_base: Mutex::new(public_base),
        plaintext: tls.is_none(),
        lockout: Mutex::new(Lockout::default()),
        ledger,
    });
    (auth, Listener { addr, tls, open })
}

/// Register a [`Listener`]'s address on `config`: plaintext, or TLS paired with a QUIC
/// listener on the same port. One function because a host listener adopts the same
/// shape and a second copy of the pairing would drift from this one.
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

/// Spawn a TCP listener with `handler`, sharing the UDS server's swansong (one shutdown
/// domain) and its server-global state — a session bind's `Arc<Session>`, a host's
/// `Arc<Host>`. `without_signals` because the UDS server already registered the
/// signal handlers.
pub(crate) fn spawn(
    listener: Listener,
    handler: impl trillium::Handler,
    swansong: trillium_tokio::Swansong,
    shared: impl Send + Sync + 'static,
) -> io::Result<trillium_server_common::ServerHandle> {
    let config = trillium_tokio::config()
        .with_swansong(swansong)
        .with_shared_state(shared)
        .without_signals()
        .listeners();
    Ok(bind(&listener, config)?.spawn(handler))
}

impl TcpAuth {
    pub(crate) fn public_base(&self) -> String {
        self.public_base
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    /// Whether the listener this authenticator fronts terminates TLS — false
    /// for the plaintext loopback dev bind (`arm`'s `tls: None`). HSTS rides
    /// only on a TLS listener (docs/web-bind.md).
    pub(crate) fn is_tls(&self) -> bool {
        !self.plaintext
    }

    /// Record the port the listener actually bound, for a bind that asked for
    /// port 0: minted links then name a port a browser can reach. A base that
    /// names a port of its own is left alone — it is the configured public URL.
    pub(crate) fn bound_port(&self, port: u16) {
        let mut base = self
            .public_base
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(unbound) = base.strip_suffix(":0") {
            *base = format!("{unbound}:{port}");
        }
    }

    /// The session cookie's name for this bind.
    pub(crate) fn cookie_name(&self) -> &'static str {
        if self.plaintext {
            auth::LOOPBACK_COOKIE_NAME
        } else {
            auth::COOKIE_NAME
        }
    }

    /// The bootstrap link: the current TOTP code in the URL *fragment*, which never
    /// reaches server or proxy logs; burning on acceptance makes scrollback and
    /// browser-history residue inert (docs/web-bind.md).
    /// A link carrying a code no one has used yet: the current step's, or when
    /// that one was just redeemed, a later step's inside the accepted skew — a
    /// second link minted in the same 30 seconds would otherwise carry a code
    /// already burned, and fail as a replay. `None` when every candidate is used.
    pub(crate) fn mint_link(&self) -> Option<String> {
        let now = auth::timestep(SystemTime::now());
        let step = (now..=now + auth::TOTP_SKEW).find(|step| !self.ledger.is_burned(*step))?;
        let code = auth::totp(&self.totp_secret, step);
        Some(format!(
            "{}/#token={}",
            self.public_base(),
            auth::format_code(code)
        ))
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
        if let Some(value) = conn.cookies().get(self.cookie_name()).map(Cookie::value)
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
    /// scrollback link; the remedy is `efference dev open`).
    Replayed,
    /// The ledger could not record the burn; verification fails closed.
    Ledger,
}

/// Which surface a gated listener serves — the one thing that differs between the
/// two binds that share this gate. A session daemon serves the control API and the
/// operator's file view; a host serves the session listing and every session's
/// control API under a `/sessions/<id>` prefix (docs/web-bind.md). Everything else
/// about the gate is identical, so the difference lives here rather than in a
/// second gate.
///
/// A surface names two paths that differ by bind rather than a set of guarded
/// prefixes: [`Surface::zone`] prices everything against the anonymous list, so
/// what a surface settles is where `/files` sits (at the root on a session bind,
/// under `/sessions/<id>` on a host), and the binding probe reaching a session
/// bind's router.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Surface {
    /// A session bind: `/v1` takes either credential, `/files` the cookie
    /// only (docs/web-bind.md).
    Session,
    /// A multi-session host: everything under `/sessions` takes either credential —
    /// the session listing and every session-prefixed control path alike — except
    /// `/sessions/<id>/files`, which takes the cookie only (docs/web-bind.md).
    Host,
}

/// What a path costs on this surface.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Zone {
    /// No credential: what [`Surface::anonymous`] names — the web client's own
    /// shell, the mint, and the binding probe.
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
    /// [`SPA_ROUTES`]. A host serves the same client at its root *and* the
    /// single-session view under `/sessions/<id>/`, which is the base that client
    /// joins its control paths onto (docs/web-client.md), so every route is answered
    /// there too. The enumeration stays exact either way: `/sessions/<id>/anything`
    /// that is not a named route is still a `404`.
    pub(super) fn spa_route(self, path: &str) -> bool {
        SPA_ROUTES.contains(&path)
            || (matches!(self, Surface::Host)
                && session_scoped(path).is_some_and(|route| SPA_ROUTES.contains(&route)))
    }

    /// What a path costs on this surface.
    ///
    /// The rule is the anonymous list inverted: everything costs a credential
    /// except what [`Self::anonymous`] names, so a route added later is guarded
    /// unless it is named there too. Stating it the other way — a list of
    /// guarded prefixes — fails the other way, leaving every new top-level route
    /// readable to anyone who can reach the bind.
    fn zone(self, method: Method, path: &str) -> Zone {
        // `/files` is guarded on the same footing as `/v1`, and more narrowly: it
        // is the operator's read surface over the working directory, so it takes
        // the cookie only (docs/web-bind.md). A bearer holder reading it would
        // bypass `[paths]`, including the deny that keeps a session out of other
        // sessions' logs. A host mounts the same surface under each session's
        // prefix, so `/sessions/<id>/files` costs the same.
        let files = match self {
            Surface::Session => under(path, "/files"),
            Surface::Host => session_scoped(path).is_some_and(|route| under(route, "/files")),
        };
        if files {
            return Zone::CookieOnly;
        }
        if self.anonymous(method, path) {
            return Zone::Anonymous;
        }
        Zone::Credential
    }

    /// Whether `path` costs nothing here. [`trillium_head::head`] runs ahead of
    /// the gate, so a HEAD arrives here already rewritten to GET.
    fn anonymous(self, method: Method, path: &str) -> bool {
        let get = method == Method::Get;
        // The web client's own shell, wherever it is served: its routes at its
        // base, the build's hashed assets, the launch images under `/splash`, and
        // the few public files whose names the build does not hash. None of it is
        // session content, and the login view lives inside it (docs/web-client.md).
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
        // The credential mint. The login flow cannot present the credential it
        // is there to obtain.
        if method == Method::Post && path == "/auth/session" {
            return true;
        }
        // The dev server's module graph, in the build that proxies one. The
        // shell is loaded before anything can authenticate, so these are the
        // requests a credential would lock the login view out behind.
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
        // and a session daemon's root would show a login form for a cookie the
        // mint would accept and nothing else would.
        matches!(self, Surface::Session) && get && path == "/sessions"
    }
}

/// Whether `path` is `prefix` itself or under it.
fn under(path: &str, prefix: &str) -> bool {
    path == prefix || path.starts_with(&format!("{prefix}/"))
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

/// The transport-side cache and TLS headers, alongside the page-hardening ones:
/// `no-store` on every `/v1` route and HSTS on a TLS listener, described with
/// the bind (docs/web-bind.md).
pub(crate) struct SecurityHeaders {
    /// Whether the listener this stack serves terminates TLS, fixed at bind
    /// time like the rest of the stack rather than read off the conn.
    tls: bool,
}

impl SecurityHeaders {
    pub(crate) fn new(tls: bool) -> Self {
        Self { tls }
    }
}

impl trillium::Handler for SecurityHeaders {
    async fn before_send(&self, conn: trillium::Conn) -> trillium::Conn {
        // Control answers are never reused from a store: on a host the same
        // address names a different session's route, and the served shape
        // changes with the build (docs/control-api.md). A session-prefixed
        // path counts as its inner route, so a host's `/sessions/<id>/v1/…`
        // is covered too. `/files` sets its own `private, no-cache` in its
        // read stage, which is what keeps the header off its change streams.
        let route = session_scoped(conn.path()).unwrap_or(conn.path());
        let conn = if under(route, "/v1") {
            conn.with_response_header(KnownHeaderName::CacheControl, "no-store")
        } else {
            conn
        };
        // RFC 6797 defines the field for secure transports only, so a
        // plaintext loopback bind does not send it. No subdomain scope and no
        // preload: the bind is one host.
        let conn = if self.tls {
            conn.with_response_header(KnownHeaderName::StrictTransportSecurity, "max-age=31536000")
        } else {
            conn
        };
        // A cross-origin document ends up neither this page's opener nor its
        // openee: the page holds a session credential, and a cross-origin
        // window must not hold a reference into its browsing-context group.
        conn.with_response_header(KnownHeaderName::CrossOriginOpenerPolicy, "same-origin")
            .with_response_header(KnownHeaderName::ContentSecurityPolicy, CSP)
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
/// graph is generated. A page that cannot load these cannot render the login
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
/// single plain segment — the same rule a host resolves ids by, so the shell is
/// offered for exactly the ids a host would accept.
fn session_scoped(path: &str) -> Option<&str> {
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

/// The web client (docs/web-client.md): `web/` built and embedded at compile time,
/// or — under the `dev-proxy` feature — proxied to a spawned Vite dev server with
/// HMR. Mounted last on a web transport, so it answers exactly the GETs the routes
/// don't claim; the gate has already let those through anonymously. A host
/// mounts the same build at its own root, which is why the index predicate is
/// surface-dependent: a host also answers the client's routes under a session
/// prefix, and the session bind does not.
pub(crate) fn frontend(surface: Surface) -> impl trillium::Handler {
    trillium_frontend::frontend!("./web")
        .with_client(crate::http::client())
        .with_index_file("index.html")
        // `trillium_frontend` takes a bare fn pointer, so the surface is chosen
        // between two fn items rather than captured in a closure.
        .with_index_predicate(match surface {
            Surface::Session => session_spa_route,
            Surface::Host => host_spa_route,
        })
}

/// A route that needs the bind's credential state: `auth` is cloned into each conn's
/// call. `None` — a socket with no TCP bind armed — answers every such route with the
/// same sentence, so a CLI verb gets a reason rather than a route mismatch.
fn with_auth<F, Fut>(auth: Option<Arc<TcpAuth>>, route: F) -> impl trillium::Handler
where
    F: Fn(Arc<TcpAuth>, Conn) -> Fut + Copy + Send + Sync + 'static,
    Fut: Future<Output = Conn> + Send + 'static,
{
    move |conn: Conn| {
        let auth = auth.clone();
        async move {
            match auth {
                Some(auth) => route(auth, conn).await,
                None => fail(
                    conn,
                    ErrorCode::NotFound,
                    "no TCP bind armed — start serve with --bind",
                ),
            }
        }
    }
}

/// The browser-facing anonymous surface of a web transport: the credential mint.
/// Mounted only by [`handler`](super::handler::handler).
pub(crate) fn mint_routes(auth: Arc<TcpAuth>) -> Router {
    Router::new().post("/auth/session", with_auth(Some(auth), mint_session))
}

/// Trade a current TOTP code for the session cookie. The one credential mint;
/// rate-limited, and every accepted code burns. The login view that reads
/// `#token=` from the fragment and POSTs here is the web client's
/// (web/src/ui/Login.tsx).
async fn mint_session(auth: Arc<TcpAuth>, mut conn: Conn) -> Conn {
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
            let cookie = Cookie::build((auth.cookie_name(), value))
                .path("/")
                .secure(!auth.plaintext)
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
                conn.with_response_header(KnownHeaderName::RetryAfter, secs.to_string()),
                ErrorBody::new(
                    ErrorCode::RateLimited,
                    format!("verification locked; retry in {secs}s"),
                ),
            )
        }
        Err(CodeRejection::Wrong) => fail(conn, ErrorCode::Unauthorized, "code did not verify"),
        Err(CodeRejection::Replayed) => fail(
            conn,
            ErrorCode::Unauthorized,
            "code already used — mint a fresh link with `efference dev open`",
        ),
        Err(CodeRejection::Ledger) => {
            fail(conn, ErrorCode::Internal, "could not record the code burn")
        }
    }
}

/// The enrollment surface, mounted only on a session's socket — possession of the host
/// machine is the enrollment authority (docs/web-bind.md). The `otpauth` URI in
/// particular carries the TOTP secret itself: a strictly stronger credential than a
/// cookie, so it must never ride a TCP listener even authenticated. With no bind
/// armed the routes exist but answer 404, so the CLI verb gets a sentence instead of
/// a route mismatch. Mounted only by [`handler`](super::handler::handler).
pub(crate) fn enrollment_routes(auth: Option<Arc<TcpAuth>>) -> Router {
    Router::new()
        .get("/auth/link", with_auth(auth.clone(), auth_link))
        .get("/auth/otpauth", with_auth(auth.clone(), auth_otpauth))
        .get("/auth/bearer", with_auth(auth, auth_bearer))
}

/// The bootstrap link: the public base with a current code in the fragment.
async fn auth_link(auth: Arc<TcpAuth>, conn: Conn) -> Conn {
    match auth.mint_link() {
        Some(url) => conn
            .with_status(200)
            .with_body(serde_json::json!({ "url": url }).to_string())
            .halt(),
        None => fail(
            conn,
            ErrorCode::RateLimited,
            "every current code has been used; ask again in half a minute",
        ),
    }
}

/// The `otpauth://` URI an authenticator app enrolls from.
async fn auth_otpauth(auth: Arc<TcpAuth>, conn: Conn) -> Conn {
    conn.with_status(200)
        .with_body(serde_json::json!({ "uri": auth.otpauth_uri() }).to_string())
        .halt()
}

/// The bearer token a program presents instead of a cookie.
async fn auth_bearer(auth: Arc<TcpAuth>, conn: Conn) -> Conn {
    conn.with_status(200)
        .with_body(serde_json::json!({ "bearer": auth.bearer() }).to_string())
        .halt()
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
