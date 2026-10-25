//! The one handler construction (docs/web-bind.md "Listener topology is the trust
//! model"). Every listener this crate binds — a session's Unix socket, a session's
//! TCP bind, a host's TCP bind — mounts [`handler`], and nothing else assembles a
//! stack. Two axes select what the stack contains: the [`Transport`] a listener is,
//! which decides the trust layers, and the [`App`] behind it, which decides the
//! routes. Every layer that is not always present is an `Option`, which trillium
//! runs as a pass-through when `None` — so the layering is structural, fixed at
//! bind time, and never a per-conn guess about which listener a request arrived on.

use super::files::Files;
use super::host::host_routes;
use super::http::control_routes;
use super::tcp::{
    SecurityHeaders, Surface, TcpAuth, enrollment_routes, frontend, gate, mint_routes,
};
use std::sync::Arc;
use trillium_caching_headers::Etag;
use trillium_logger::{Logger, Target};

/// Which listener a stack is mounted on, and therefore what it trusts.
pub enum Transport {
    /// The session's Unix socket: the operator's own process boundary. No gate, no
    /// browser hardening, no web client. The enrollment routes (`/auth/link`,
    /// `/auth/otpauth`, `/auth/devices`) are mounted here and only here — possession
    /// of the host machine is the enrollment authority (docs/web-bind.md); they answer
    /// `404` with a sentence when no TCP bind is armed.
    Socket { auth: Option<Arc<TcpAuth>> },
    /// A TCP listener: the hardening + auth gate ahead of every route, the credential
    /// mint, and the web client behind the routes (docs/web-bind.md).
    Web { auth: Arc<TcpAuth> },
}

/// What sits behind the transport layers.
pub enum App {
    /// One session's control API (docs/control-api.md), plus — on a web transport —
    /// the operator's read surface over the working directory (`/files`).
    Session { files: Files },
    /// A multi-session host's listener (docs/control-channel.md): the session
    /// list, `POST /sessions`, and each session's control API under
    /// `/sessions/<id>/`. The gate guards `/sessions`, and the web client is also
    /// served under a session prefix (docs/web-bind.md). The server-global state
    /// is the `Arc<Host>`.
    Host,
}

impl App {
    /// A session over its explicitly supplied working directory.
    pub fn session(cwd: impl AsRef<std::path::Path>) -> Self {
        Self::Session {
            files: Files::rooted_at(cwd),
        }
    }

    fn surface(&self) -> Surface {
        match self {
            Self::Session { .. } => Surface::Session,
            Self::Host => Surface::Host,
        }
    }
}

/// The handler stack, in the one order every listener uses. Tuple order is run
/// order; `before_send` runs in reverse, which is why the response-header layers
/// sit first.
pub fn handler(transport: Transport, app: App) -> impl trillium::Handler {
    let surface = app.surface();
    let (socket_auth, web_auth) = match transport {
        Transport::Socket { auth } => (Some(auth), None),
        Transport::Web { auth } => (None, Some(auth)),
    };
    let (files, is_host) = match app {
        App::Session { files } => (Some(files), false),
        App::Host => (None, true),
    };
    let web = web_auth.is_some();
    (
        Logger::new().with_target(Target::Logger(log::Level::Info)),
        // Outermost, so its `before_send` runs last and the headers land on every
        // response the stack produces — including the routers' errors. On every
        // transport, not only the browser-facing one: `nosniff` is half of what
        // makes serving a repository's `.html` as `text/plain` safe, and setting
        // it everywhere is cheaper than reasoning about whether some consumer of
        // the Unix socket renders what it reads. HSTS is the one
        // listener-conditional: only a TLS web bind sends it.
        SecurityHeaders::new(web_auth.as_ref().is_some_and(|auth| auth.is_tls())),
        // HEAD support: rewrites to GET on the way in, strips the body (keeping
        // content-length) on the way out. Before the gate, so it only ever sees GET.
        trillium_head::head(),
        web_auth.clone().map(|auth| gate(auth, surface)),
        // A content-hash `etag` on every successful GET with a static body, and a
        // matching `if-none-match` answered `304` — the whole caching story for the
        // single-entry reads (docs/control-api.md). `cache-control` is `SecurityHeaders`'
        // (`no-store` on `/v1`): the served shape is the build's, not the log's, and
        // at a socket root the same address names a different entry in every session,
        // so the validator's revalidation is the only reuse path.
        Etag::new(),
        socket_auth.map(enrollment_routes),
        web_auth.map(mint_routes),
        files.is_some().then(control_routes),
        is_host.then(|| host_routes(web)),
        // The operator's read surface over the working directory (docs/web-bind.md),
        // mounted only behind a gate so the socket never serves it. A router over
        // `/files/**`, so every other path passes through to the routes below.
        web.then_some(files).flatten().map(Files::handler),
        // Last: any GET the routes did not claim is the web client's surface
        // (docs/web-client.md) — an asset, or the SPA index.
        web.then(|| frontend(surface)),
    )
}
