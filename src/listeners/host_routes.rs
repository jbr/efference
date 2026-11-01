//! The host's route table, and the stage that resolves `/sessions/:session/*` into conn
//! state for the session's control routes (docs/control-api.md). The handlers it mounts
//! are `host_http.rs`.

use super::*;
use crate::host::entries::Entry;
use crate::routes::host_http::{
    catalog_all, catalog_one, create_session, end_session, forget_project, host_of,
    inference_inventory, list_projects, list_sessions, missing_host, no_such_session,
    patch_session, preview_session, read_session, report_attention,
};
use crate::routes::http::control_routes;
use trillium::Conn;
use trillium_router::Router;
use trillium_router::RouterConnExt;

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
                move |conn: Conn| async move { resolve_session(conn, files) },
                control_routes(),
                files.then(files::route),
            ),
        );
    router.post("/sessions", create_session)
}

/// `/sessions/:session/*`: put the session into conn state for the control
/// routes, or `404` when no hosted session has that id. Exact public web-shell
/// routes pass through even without a session (docs/web-client.md).
fn resolve_session(conn: Conn, web: bool) -> Conn {
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
            // The view `files::route` serves, one route for every session.
            .with_state(hosted.files),
        Some(Entry::Starting(starting)) => conn.with_state(starting),
        // The router has stripped the session prefix. These exact routes carry
        // only the public shell; the client then observes the API's 404 and goes
        // home (docs/web-client.md). Control and file paths still fail here.
        None if web
            && conn.method() == trillium::Method::Get
            && tcp::Surface::Session
                .spa_route(&format!("/{}", conn.path().trim_start_matches('/'))) =>
        {
            conn
        }
        None => no_such_session(conn, &id),
    }
}
