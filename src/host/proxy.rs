//! Forwarding one session's control API through the host (docs/host.md).
//!
//! `/sessions/<id>/…` is proxied to `<base>/<id>/control.sock` byte-for-byte: the
//! host adds no endpoint that translates the per-session API, so what a client joins
//! onto its base is the API docs/control-api.md documents, unchanged.
//!
//! Two properties are why this is `trillium-proxy` rather than a hand-rolled
//! forward. The upstream response rides through as a *streaming* body, so
//! `GET /v1/events` flushes each frame as it arrives instead of buffering the
//! connection's lifetime; and a `101` is carried as an upgrade both halves of which
//! are then copied full-duplex, so an upgrade-shaped transport would need nothing
//! here.

use super::*;
use std::collections::HashMap;
use std::sync::Mutex;
use trillium::{Handler, Status, Upgrade};
use trillium_proxy::{Proxy, Url, upstream::UpstreamSelector};
use trillium_router::RouterConnExt;

/// Where a proxied request goes: the path *after* the `/sessions/<id>` prefix, on a
/// placeholder authority.
///
/// The authority is a placeholder because the connector dials a fixed socket path
/// and ignores it — the URL supplies only the request target and the `Host` header
/// (`UnixClientConfig`, docs/control-channel.md).
///
/// It reads [`Conn::path`], which is the router's wildcard remainder, rather than
/// `path_and_query`, which is the untrimmed request path: forwarding the latter
/// would send the session `/sessions/<id>/v1/state`, a path its router does not
/// serve.
#[derive(Debug)]
struct SessionUpstream;

impl UpstreamSelector for SessionUpstream {
    fn determine_upstream(&self, conn: &mut Conn) -> Option<Url> {
        let mut url = Url::parse("http://localhost/").ok()?;
        url.set_path(conn.path());
        let query = conn.querystring();
        url.set_query((!query.is_empty()).then_some(query));
        Some(url)
    }
}

/// The `/sessions/<id>/*` handler: one [`Proxy`] per session, built on first use and
/// kept, so a session's requests share one client and its connection pool instead of
/// dialing a fresh socket per request.
pub(crate) struct SessionProxy {
    sessions: Sessions,
    proxies: Mutex<HashMap<String, Arc<Proxy<SessionUpstream>>>>,
}

impl SessionProxy {
    pub(crate) fn new(sessions: Sessions) -> Self {
        Self {
            sessions,
            proxies: Mutex::new(HashMap::new()),
        }
    }

    /// The proxy for `id`, built if this is the first request for it. `None` when the
    /// id is not a plain path segment or its session dir holds no socket — existence
    /// is checked here so a nonexistent id is a `404` rather than a dial failure.
    fn proxy_for(&self, id: &str) -> Option<Arc<Proxy<SessionUpstream>>> {
        let mut proxies = self.proxies.lock().unwrap();
        if let Some(proxy) = proxies.get(id) {
            return Some(Arc::clone(proxy));
        }
        let socket = self.sessions.socket(id)?;
        if !socket.exists() {
            return None;
        }
        let client = trillium_client::Client::new(trillium_tokio::UnixClientConfig::new(socket));
        let proxy = Arc::new(
            Proxy::new(client, SessionUpstream)
                // Forward the session's own `404` body — the standard error envelope
                // (docs/control-api.md) — instead of passing the conn through
                // unclaimed, which would leave the web client's frontend handler to
                // answer a control-API request.
                .proxy_not_found()
                .with_websocket_upgrades()
                .with_via_pseudonym("efference-host"),
        );
        proxies.insert(id.to_string(), Arc::clone(&proxy));
        Some(proxy)
    }

    /// The proxy already built for the session this upgrade belongs to. The upgrade
    /// path is the whole request path (the router does not trim it for upgrades), so
    /// the id is read from it directly. Nothing is built here: an upgrade only ever
    /// follows the response that produced it.
    fn proxy_for_upgrade(&self, upgrade: &Upgrade) -> Option<Arc<Proxy<SessionUpstream>>> {
        let id = upgrade
            .path()
            .strip_prefix("/sessions/")?
            .split('/')
            .next()?;
        self.proxies.lock().unwrap().get(id).map(Arc::clone)
    }
}

impl Handler for SessionProxy {
    async fn run(&self, conn: Conn) -> Conn {
        let Some(id) = conn.param("id").map(str::to_string) else {
            return fail(
                conn,
                ErrorCode::Internal,
                "session route matched without an id",
            );
        };
        let Some(proxy) = self.proxy_for(&id) else {
            return no_such_session(conn, &id);
        };
        let conn = proxy.run(conn).await;
        // The session dir held a socket but nothing was listening on it: the daemon
        // exited between the scan and this request. Answered as the same `404` an
        // unknown id gets — liveness and existence are one question to a client, and
        // the recovery is the same re-read of `GET /sessions`. (The session's own
        // routes never answer `502`, so nothing else lands here.)
        if conn.status() == Some(Status::BadGateway) {
            return no_such_session(conn, &id);
        }
        conn
    }

    fn has_upgrade(&self, upgrade: &Upgrade) -> bool {
        self.proxy_for_upgrade(upgrade)
            .is_some_and(|proxy| proxy.has_upgrade(upgrade))
    }

    async fn upgrade(&self, upgrade: Upgrade) {
        if let Some(proxy) = self.proxy_for_upgrade(&upgrade) {
            proxy.upgrade(upgrade).await;
        }
    }

    fn name(&self) -> std::borrow::Cow<'static, str> {
        format!("session proxy ({})", self.sessions.base().display()).into()
    }
}
