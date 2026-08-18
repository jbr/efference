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
    let config = match &listener.tls {
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
            )?,
        None => config.bind_tcp(listener.addr.as_str())?,
    };
    Ok(config.spawn(handler))
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

    /// Whether the conn presents a valid credential: the session cookie, or the
    /// bearer for programs (which can set headers where `EventSource` cannot).
    fn authed(&self, conn: &Conn) -> bool {
        // Read through the jar `CookiesHandler` parsed, never `get_str` on the
        // raw header: the cookie header arrives as several field lines when an
        // h2/h3 client splits one line per cookie (RFC 9113 §8.2.3), and
        // because cookies ignore ports, any other service's cookie on this
        // host rides along and forces the split — `get_str` reads none of a
        // multi-valued header, which presented as browsers (with company on
        // the host) failing auth that curl (one cookie, one line) passed.
        if let Some(value) = conn.cookies().get(auth::COOKIE_NAME).map(Cookie::value)
            && auth::cookie_is_valid(&self.cookie_key, value, SystemTime::now())
        {
            return true;
        }
        if let Some(authorization) = conn
            .request_headers()
            .get_str(KnownHeaderName::Authorization)
            && let Some(token) = authorization.strip_prefix("Bearer ")
            && self.root.bearer_matches(token.trim())
        {
            return true;
        }
        false
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

/// The hardening + auth gate, run before every route on the TCP listener
/// (docs/web-bind.md): Host allowlist (DNS rebinding), Origin + Content-Type checks on
/// unsafe methods (CSRF — a localhost or LAN bind is not a trust boundary while a
/// browser is present), then the credential check. The anonymous surface is the web
/// client's shell (every GET outside `/v1`) and the credential mint itself.
fn gate(auth: Arc<TcpAuth>) -> impl trillium::Handler {
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
            // `trillium_head::head()` runs ahead of the gate, so a HEAD arrives
            // here already rewritten to GET. Anonymous surface: every GET outside
            // `/v1` — the web client's shell and hashed assets, same posture as the
            // inline bootstrap page before it (docs/web-client.md) — plus the
            // credential mint. `/v1` is the whole sensitive surface on this
            // listener; a non-GET outside it fails closed here rather than 404ing.
            let path = conn.path();
            let v1 = path == "/v1" || path.starts_with("/v1/");
            let anonymous = match method {
                Method::Get => !v1,
                Method::Post => path == "/auth/session",
                _ => false,
            };
            if !anonymous && !auth.authed(&conn) {
                return fail(
                    conn,
                    ErrorCode::Unauthorized,
                    "no valid session cookie or bearer; POST /auth/session with a current code",
                );
            }
            conn
        }
    }
}

/// The handler stack the TCP listener mounts, in one place so the tests exercise
/// the same one rather than a copy that can drift from it.
pub(crate) fn stack(auth: Arc<TcpAuth>) -> impl trillium::Handler {
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
        gate(Arc::clone(&auth)),
        public_router(auth),
        router(),
        // Last: any GET neither router claimed is the web client's surface
        // (docs/web-client.md) — an asset, or the SPA index.
        frontend(),
    )
}

/// The policy the embedded build is served under. Everything the page loads is
/// same-origin and external — one module script and one stylesheet, both hashed,
/// with the grammar chunks imported dynamically from the same origin — so `'self'`
/// needs no hash, nonce, or `unsafe-` escape. Syntax highlighting needs no
/// `style-src` accommodation: token colours are set through React's `style` prop,
/// which is CSSOM, and CSP governs inline `<style>` elements and `style=""`
/// attributes parsed from markup rather than CSSOM mutations
/// (backlog/web/untrusted-output.md).
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
/// into markup (backlog/web/untrusted-output.md): every body on this page is model-
/// or tool-authored, and the page holds a credential for a channel that runs
/// commands. `frame-ancestors 'none'` also keeps the session out of an iframe,
/// which no surface here wants.
struct SecurityHeaders;

impl trillium::Handler for SecurityHeaders {
    async fn before_send(&self, conn: trillium::Conn) -> trillium::Conn {
        conn.with_response_header(KnownHeaderName::ContentSecurityPolicy, CSP)
            .with_response_header("x-content-type-options", "nosniff")
            .with_response_header(KnownHeaderName::ReferrerPolicy, "no-referrer")
    }
}

/// The web client (docs/web-client.md): `web/` built and embedded at compile time,
/// or — under the `dev-proxy` feature — proxied to a spawned Vite dev server with
/// HMR. Mounted after both routers on the TCP spawn, so it answers exactly the GETs
/// they don't claim; the gate has already let those through anonymously. The SPA
/// index serves only at `/` — the client routes nothing else yet, and answering
/// every unmatched path with the index would hand scanner probes a `200`.
fn frontend() -> impl trillium::Handler {
    trillium_frontend::frontend!("./web")
        .with_client(trillium_client::Client::new(
            trillium_tokio::ClientConfig::default(),
        ))
        .with_index_file("index.html")
        .with_index_predicate(|conn| conn.path() == "/")
}

/// The browser-facing anonymous surface, mounted only on the TCP spawn: the
/// credential mint. Static assets fall through to [`frontend`]; everything else on
/// TCP is the shared `/v1` router behind the gate.
pub(crate) fn public_router(auth: Arc<TcpAuth>) -> Router {
    Router::new()
        // Trade a current TOTP code for the session cookie. The one credential mint;
        // rate-limited, and every accepted code burns. The login view that reads
        // `#token=` from the fragment and POSTs here is the web client's
        // (web/src/ui/Login.tsx), successor to the inline bootstrap page.
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
