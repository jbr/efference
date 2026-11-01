//! The host's own handlers (docs/control-api.md): the session list and archiving, the
//! inference inventory and attention reports, and the helpers every handler of the host
//! shares. The families with a file of their own are in `host_http/`; the table that
//! mounts them is `host_routes.rs`.

use super::*;
use crate::host::Ended;
use crate::host::Host;
use crate::host::listing::listing_stream;
use crate::protocol::answers::InventoryList;
use crate::routes::errors::ErrorCode;
use crate::routes::errors::fail;
use serde::Deserialize;
use serde::Serialize;
use serde_json::Value;
use std::sync::Arc;
use trillium::Conn;
use trillium_router::RouterConnExt;
use trillium_sse::sse;

mod archived;
use archived::archived_listing;
pub(crate) use archived::read_session;

mod catalog;
pub(crate) use catalog::{catalog_all, catalog_one};

mod create;
pub(crate) use create::create_session;

mod options;
pub(crate) use options::preview_session;

mod patch;
pub(crate) use patch::patch_session;

mod projects;
pub(crate) use projects::{forget_project, list_projects};

pub(crate) fn host_of(conn: &Conn) -> Option<Arc<Host>> {
    conn.shared_state::<Arc<Host>>().map(Arc::clone)
}

pub(crate) fn missing_host(conn: Conn) -> Conn {
    fail(conn, ErrorCode::Internal, "missing host state")
}

/// `GET /inference`: configured host connections, sorted by name. The registry is
/// loaded once at host startup, so this never reads config or credentials per request.
pub(crate) async fn inference_inventory(conn: Conn) -> Conn {
    match host_of(&conn) {
        Some(host) => views::answer(
            conn,
            200,
            InventoryList {
                connections: host.inference.inventory(),
            },
        ),
        None => missing_host(conn),
    }
}

/// `POST /attention`: one client's report of whether a person is attending, and
/// which session it has open (`attention.rs`, docs/web-push.md). `204` on
/// success; a malformed body is a `400` naming the problem.
pub(crate) async fn report_attention(mut conn: Conn) -> Conn {
    let Some(host) = host_of(&conn) else {
        return missing_host(conn);
    };
    let body = conn.request_body_string().await.unwrap_or_default();
    match crate::protocol::attention::AttentionReport::parse(&body) {
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
pub(crate) fn list_sessions() -> impl trillium::Handler {
    (
        archived_listing,
        sse(|conn: &mut Conn| match host_of(conn) {
            Some(host) => listing_stream(Arc::downgrade(&host)),
            // Unreachable through the mounted router, which always carries the
            // host as shared state; an empty stream closes rather than hangs.
            None => Box::pin(futures_lite::stream::empty()),
        })
        .with_heartbeat(http::EVENTS_HEARTBEAT),
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

/// `DELETE /sessions/:session`: archive the session. The same transition as
/// `PATCH {"state": "archived"}`, in the imperative spelling, and answering once
/// teardown has run — background children reaped, scratch removed — so a client
/// that then lists sessions or reads the dir sees the finished state.
///
/// Archiving rather than ending is what it does: the runtime stops, and the
/// directory and log stay, so the session is resumable by patching it `active`
/// again (docs/log-and-projection.md).
pub(crate) async fn end_session(mut conn: Conn) -> Conn {
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
    let mut answer = ended.answer(id);
    answer.ended_with = ending_with.to_vec();
    views::answer(conn, 200, answer)
}

pub(crate) fn no_such_session(conn: Conn, id: &str) -> Conn {
    fail(
        conn,
        ErrorCode::NotFound,
        format!("no hosted session {id:?}; read GET /sessions for the current list"),
    )
}
