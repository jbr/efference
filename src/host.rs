//! The host process — one front door for every session on a machine (docs/host.md).
//!
//! `efference host` binds one authenticated listener and serves three things: the
//! live-session listing, the per-session proxy that forwards `/sessions/<id>/…` to
//! that session's Unix socket, and the web client at the root. The listener and its
//! credential layer are the ones the single-session bind already ships
//! (docs/web-bind.md): the host adopts [`TcpBind`], [`arm`](crate::daemon::tcp), and
//! the gate rather than shipping a second, unauthenticated bind.
//!
//! The host starts nothing and owns nothing: sessions are separate `efference serve`
//! processes, discovered by the same liveness scan the CLI verbs run, and their
//! sockets stay UDS-only. Adding, stopping, and adopting sessions are not built
//! (docs/host.md).

use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use trillium::Conn;
use trillium_cookies::CookiesHandler;
use trillium_router::Router;

use crate::daemon::errors::{ErrorCode, fail};
use crate::daemon::tcp::{
    self, SecurityHeaders, Surface, TcpAuth, TcpBind, frontend, gate, public_router,
};
use crate::daemon::{SESSION_BASE, SOCKET_NAME};

mod proxy;

#[cfg(test)]
mod test;

/// What `efference host` resolved to: the listener + credentials (shared with
/// `serve --bind`) and the directory session dirs live in.
pub struct Host {
    /// The listener and the machine root every credential derives from.
    pub bind: TcpBind,
    /// The session base directory to scan and to resolve ids against — `.efference`
    /// under the process's working directory unless a caller says otherwise.
    pub base: PathBuf,
}

impl Host {
    /// A host over the standard session base beneath the current working directory.
    pub fn new(bind: TcpBind) -> Self {
        Self {
            bind,
            base: PathBuf::from(SESSION_BASE),
        }
    }
}

/// Bind the host listener and serve until shutdown. Signals are trillium's: this
/// process has one server, so its `Swansong` is the whole shutdown domain.
pub async fn serve(host: Host) -> io::Result<()> {
    let Host { bind, base } = host;
    let open = bind.open;
    let (auth, listener) = tcp::arm(bind);
    let addr = listener.addr.clone();
    let handler = stack(Arc::clone(&auth), base);
    let handle = tcp::bind(&listener, trillium_tokio::config().listeners())?.spawn(handler);
    let link = auth.mint_link();
    eprintln!("host: {} (bound {addr})", auth.public_base());
    eprintln!("  {link}");
    // Wait for the listener to answer before handing the link to a browser.
    handle.info().await;
    if open && webbrowser::open(&link).is_err() {
        eprintln!("[could not open a browser for {link}]");
    }
    handle.await;
    Ok(())
}

/// The handler stack the host listener mounts. Same shape and same order as the
/// session bind's (docs/web-bind.md), differing only in what sits between the gate
/// and the web client: session routes instead of one session's `/v1`.
pub(crate) fn stack(auth: Arc<TcpAuth>, base: PathBuf) -> impl trillium::Handler {
    (
        // Outermost, so its `before_send` runs last and the headers land on every
        // response the stack produces.
        SecurityHeaders,
        // HEAD rewritten to GET going in, body stripped coming out. Before the gate,
        // so it only ever sees GET.
        trillium_head::head(),
        CookiesHandler::new(),
        gate(Arc::clone(&auth), Surface::Host),
        // The credential mint, anonymous by the gate's reckoning: the host's login
        // page is the same web client login view.
        public_router(auth),
        router(Sessions::new(base)),
        // Last: any GET the routes didn't claim is the web client's surface — an
        // asset, the SPA index at the root, or the same index under a session
        // prefix, which is the single-session view's base.
        frontend(Surface::Host),
    )
}

/// The host's own routes. Everything under `/sessions/<id>/` is the session's API
/// unchanged (docs/control-api.md); the host adds no endpoint that translates or
/// enriches it.
pub(crate) fn router(sessions: Sessions) -> Router {
    let listing = sessions.clone();
    Router::new()
        .get("/sessions", move |conn: Conn| {
            let sessions = listing.clone();
            async move {
                let live = sessions.live().await;
                let body = serde_json::to_string(&live).unwrap_or_else(|_| "[]".into());
                conn.with_status(200).with_body(body).halt()
            }
        })
        // The web client's own routes under a session prefix — the single-session
        // view and its file tree, at the base that client joins control paths onto.
        // Named here for two reasons: the proxy must not forward them to a session
        // (whose router serves the control API, not the client), and `frontend`
        // below must be the one to answer them. These handlers therefore claim the
        // path from the proxy and pass the conn through unhalted. The set matches
        // `Surface::spa_route`'s (src/daemon/tcp.rs) and the service worker's
        // (web/public/sw.js); a route in one and not the others is a route that
        // works in one binding and 404s in another.
        .get("/sessions/:id", client_shell)
        .get("/sessions/:id/", client_shell)
        .get("/sessions/:id/tree", client_shell)
        // `all` covers the five methods the control API uses. The trailing wildcard
        // is what the proxy forwards: the router pushes the remainder onto the conn's
        // path, so `/sessions/x/v1/state` reaches the session as `/v1/state`.
        .all("/sessions/:id/*", proxy::SessionProxy::new(sessions))
}

/// A client route under a session prefix: matched so the proxy does not claim it,
/// answered by [`frontend`] with the same SPA shell `/` and `/tree` get.
async fn client_shell(conn: Conn) -> Conn {
    conn
}

/// The session directory layout as the host reads it: `<base>/<id>/control.sock`
/// (docs/control-channel.md). Cheap to clone — the routes each hold one.
#[derive(Clone)]
pub(crate) struct Sessions {
    base: Arc<PathBuf>,
}

/// One entry of `GET /sessions`. The listing carries what the liveness scan already
/// yields and nothing else: a session's own state is read through its own API, at
/// `/sessions/<id>/v1/state` (docs/host.md).
#[derive(serde::Serialize, serde::Deserialize, Debug, PartialEq, Eq)]
pub struct SessionEntry {
    /// The session dir's name — `serve --name`, or the generated timestamp id. The
    /// path segment a client joins its control base onto.
    pub id: String,
}

/// Whether `id` is a single plain path segment — ASCII alphanumerics, `-`, `_`, `.`,
/// and neither `.` nor `..` (docs/host.md). Anything else names no session: the id
/// arrives from a URL and is joined onto a filesystem path, so it is validated
/// before the join rather than trusted. The client-route predicate
/// (`Surface::spa_route`, src/daemon/tcp.rs) asks the same question, so the shell is
/// served for exactly the ids the proxy would accept.
pub(crate) fn plain_id(id: &str) -> bool {
    !id.is_empty()
        && id != "."
        && id != ".."
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
}

impl Sessions {
    pub(crate) fn new(base: PathBuf) -> Self {
        Self {
            base: Arc::new(base),
        }
    }

    /// The control socket for `id`, or `None` when the id is not a single plain path
    /// segment. Validated here rather than trusted: the id arrives from a URL and is
    /// joined onto a filesystem path, so anything that could name a directory other
    /// than one session's is refused before the join.
    pub(crate) fn socket(&self, id: &str) -> Option<PathBuf> {
        plain_id(id).then(|| self.base.join(id).join(SOCKET_NAME))
    }

    /// The live sessions, sorted by id. **Liveness, not existence** — the same scan
    /// the CLI verbs run (docs/control-channel.md): a successful connect means a
    /// daemon is listening, while a stale dir from a dead one refuses and is left
    /// out. A dead session's log stays on disk and readable by the offline verbs;
    /// what this endpoint answers is which sessions can be talked to now.
    pub(crate) async fn live(&self) -> Vec<SessionEntry> {
        let mut live = Vec::new();
        let Ok(entries) = std::fs::read_dir(&*self.base) else {
            return live;
        };
        for entry in entries.flatten() {
            let Some(id) = entry.file_name().to_str().map(str::to_string) else {
                continue;
            };
            let Some(socket) = self.socket(&id) else {
                continue;
            };
            if tokio::net::UnixStream::connect(&socket).await.is_ok() {
                live.push(SessionEntry { id });
            }
        }
        live.sort_by(|a, b| a.id.cmp(&b.id));
        live
    }

    /// The base directory, for error prose.
    pub(crate) fn base(&self) -> &Path {
        &self.base
    }
}

/// The one refusal both the listing and the proxy share: an id that names no live
/// session. Existence and liveness collapse into one answer on purpose — a client
/// holding an id the scan no longer returns gets the same `404` whether the session
/// dir is gone or its daemon has exited, and re-reads `GET /sessions` either way.
fn no_such_session(conn: Conn, id: &str) -> Conn {
    fail(
        conn,
        ErrorCode::NotFound,
        format!("no live session {id:?}; read GET /sessions for the current list"),
    )
}
