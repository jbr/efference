//! The host's own routes, and the stage that resolves `/sessions/:session/*` into
//! conn state for the session's control routes (docs/control-api.md).

use super::*;

/// The host's routes (docs/control-api.md). `POST /sessions` is mounted on every
/// host listener, socket and network alike: creation's environment is
/// established per request (docs/env.md), so what it needs is the hook or the
/// host's own baseline rather than a local caller. `files` serves each session's
/// `/files` under its prefix — the web transport, as on a session bind.
pub(crate) fn host_routes(files: bool) -> impl trillium::Handler {
    let router = Router::new()
        .get("/inference", inference_inventory)
        .get("/inference/catalog", catalog_all)
        .get("/inference/catalog/:connection", catalog_one)
        .with_route(trillium::Method::Query, "/sessions", preview_session)
        .get("/sessions", list_sessions())
        // Registered before the `/sessions/:session/*` wildcard below, which would
        // resolve the session into conn state and 404 a patch that is what
        // makes it live in the first place (and a read of a dormant session).
        .get("/sessions/:session", read_session)
        .patch("/sessions/:session", patch_session)
        .delete("/sessions/:session", end_session)
        // Where sessions have been started, and forgetting one. Registered
        // before the `/sessions/:session/*` wildcard below, which is a different
        // prefix but the same reader of a path's first segment.
        .get("/projects", list_projects)
        // Clients report whether a person is attending (`attention.rs`).
        .post("/attention", report_attention)
        .delete("/projects", forget_project)
        // The wildcard remainder becomes the conn's path, so the control routes
        // match `/v1/…` exactly as they do at a session bind's root.
        //
        // The session segment is `:session`, never `:id`: the wildcard rewrites
        // the *path* but leaves this capture in the conn's params, and a
        // same-named capture inside `control_routes` (`/v1/processes/:id`,
        // `/v1/notifications/:id`) would then read the session name where the
        // route means an ordinal — every such route answering 400
        // "process id must be an integer" for a well-formed id.
        .all(
            "/sessions/:session/*",
            (
                move |conn: Conn| async move { resolve_session(conn, files).await },
                control_routes(),
                files.then(Files::route),
            ),
        );
    router.post("/sessions", create_session)
}

pub(super) fn host_of(conn: &Conn) -> Option<Arc<Host>> {
    conn.shared_state::<Arc<Host>>().map(Arc::clone)
}

pub(super) fn missing_host(conn: Conn) -> Conn {
    fail(conn, ErrorCode::Internal, "missing host state")
}

/// `GET /inference`: configured host connections, sorted by name. The registry is
/// loaded once at host startup, so this never reads config or credentials per request.
async fn inference_inventory(conn: Conn) -> Conn {
    match host_of(&conn) {
        Some(host) => conn
            .with_status(200)
            .with_body(serde_json::json!({ "connections": host.inference.inventory() }).to_string())
            .halt(),
        None => missing_host(conn),
    }
}

/// `POST /attention`: one client's report of whether a person is attending, and
/// which session it has open (`attention.rs`, docs/web-push.md). `204` on
/// success; a malformed body is a `400` naming the problem.
async fn report_attention(mut conn: Conn) -> Conn {
    let Some(host) = host_of(&conn) else {
        return missing_host(conn);
    };
    let body = conn.request_body_string().await.unwrap_or_default();
    match super::attention::AttentionReport::parse(&body) {
        Ok(report) => {
            host.report_attention(&report);
            conn.with_status(204).halt()
        }
        Err(message) => fail(conn, ErrorCode::BadRequest, message),
    }
}

/// `GET /sessions`: the listing (`listing.rs`) — one row per hosted session,
/// sorted by id.
///
/// The route is a tuple so the same URL serves both readings of the one
/// resource. The `Sse` half negotiates on `Accept`: a request naming
/// `text/event-stream` subscribes to the listing and receives it whole on every
/// change, and any other request passes through untouched to the JSON half
/// below (docs/control-api.md). A `state` query is read first (`archived.rs`):
/// `archived` answers the dormant listing as JSON whatever the `Accept`.
fn list_sessions() -> impl trillium::Handler {
    (
        archived_listing,
        sse(|conn: &mut Conn| match host_of(conn) {
            Some(host) => listing_stream(Arc::downgrade(&host)),
            // Unreachable through the mounted router, which always carries the
            // host as shared state; an empty stream closes rather than hangs.
            None => Box::pin(futures_lite::stream::empty()),
        })
        .with_heartbeat(super::http::EVENTS_HEARTBEAT),
        read_sessions,
    )
}

/// The JSON half of [`list_sessions`].
async fn read_sessions(conn: Conn) -> Conn {
    match host_of(&conn) {
        Some(host) => conn.with_status(200).with_body(host.listing_json()).halt(),
        None => missing_host(conn),
    }
}

/// `/sessions/:session/*`: put the session into conn state for the control
/// routes, or `404` when no hosted session has that id. Exact public web-shell
/// routes pass through even without a session (docs/web-client.md).
async fn resolve_session(conn: Conn, web: bool) -> Conn {
    let Some(host) = host_of(&conn) else {
        return missing_host(conn);
    };
    let id = conn.param("session").unwrap_or_default().to_string();
    // A starting session has no `Session` yet: the control routes that serve it read
    // the entry, and the rest answer `409 starting` (`http.rs`). One that failed is
    // served the same way for a while, so its reason reaches a late client.
    let entry = host
        .entry(&id)
        .or_else(|| host.recently_failed(&id).map(Entry::Starting));
    match entry {
        Some(Entry::Hosted(hosted)) => conn
            .with_state(hosted.session)
            // The view `Files::route` serves, one route for every session.
            .with_state(hosted.files),
        Some(Entry::Starting(starting)) => conn.with_state(starting),
        // The router has stripped the session prefix. These exact routes carry
        // only the public shell; the client then observes the API's 404 and goes
        // home (docs/web-client.md). Control and file paths still fail here.
        None if web
            && conn.method() == trillium::Method::Get
            && super::tcp::Surface::Session
                .spa_route(&format!("/{}", conn.path().trim_start_matches('/'))) =>
        {
            conn
        }
        None => no_such_session(conn, &id),
    }
}

/// `DELETE /sessions/:session`: archive the session. The same transition as
/// `PATCH {"state": "archived"}`, in the imperative spelling, and answering once
/// teardown has run — background children reaped, scratch removed — so a client
/// that then lists sessions or reads the dir sees the finished state.
///
/// Archiving rather than ending is what it does: the runtime stops, and the
/// directory and log stay, so the session is resumable by patching it `active`
/// again (docs/log-and-projection.md).
async fn end_session(mut conn: Conn) -> Conn {
    let Some(host) = host_of(&conn) else {
        return missing_host(conn);
    };
    let id = conn.param("session").unwrap_or_default().to_string();
    // The body is optional: a client that says nothing about itself archives as the
    // operator, and the entry records no source (docs/control-api.md).
    let body = conn.request_body_string().await.unwrap_or_default();
    let source = if body.trim().is_empty() {
        None
    } else {
        match serde_json::from_str::<ArchiveOrigin>(&body) {
            Ok(origin) => origin.source,
            Err(error) => {
                return fail(
                    conn,
                    ErrorCode::BadRequest,
                    format!("bad DELETE /sessions/:session body: {error}"),
                );
            }
        }
    };
    // Read before the end starts, while the children it cascades to are still listed.
    let ending_with = host.ending_with(&id);
    match host.end_by(&id, source) {
        Some(completion) => {
            let ended = completion.await;
            archived(conn, &id, ended, &ending_with)
        }
        None => no_such_session(conn, &id),
    }
}

/// The optional body of `DELETE /sessions/:session`: who is archiving, spelled as a
/// turn's `source` is (`web`, `mcp[<name>]`, `session[<id>]`) and recorded on the
/// `Archived` entry (docs/log-and-projection.md).
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct ArchiveOrigin {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
}

/// The `200` an archive answers with, carrying `worktree` when the session had its own
/// and `ended_with` when sessions it created end with it (docs/control-api.md).
fn archived(conn: Conn, id: &str, ended: Ended, ending_with: &[String]) -> Conn {
    let mut body = serde_json::json!({ "id": id, "state": ended.state() });
    if let Some(worktree) = ended.retired.and_then(|retired| retired.response()) {
        body["worktree"] = worktree;
    }
    if !ending_with.is_empty() {
        body["ended_with"] = serde_json::json!(ending_with);
    }
    conn.with_status(200).with_body(body.to_string()).halt()
}

pub(super) fn no_such_session(conn: Conn, id: &str) -> Conn {
    fail(
        conn,
        ErrorCode::NotFound,
        format!("no hosted session {id:?}; read GET /sessions for the current list"),
    )
}
