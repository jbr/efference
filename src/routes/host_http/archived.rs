//! `GET /sessions?state=archived` and `GET /sessions/:session`: the sessions on disk that nothing here is
//! running (docs/control-api.md).
//!
//! The same collection as the live listing, filtered by the lifecycle word
//! [`SessionState`](crate::protocol::sessions::SessionState) already names. The rows are
//! `host/archived.rs`'s.

use super::*;
use crate::protocol::sessions::DormantSession;
use std::path::PathBuf;

/// The roots an archived read covers: the `cwd` query when given, else every
/// remembered root, most recently used first.
fn scan_roots(host: &Host, query: &str) -> Result<Vec<PathBuf>, &'static str> {
    match http::query_value(query, "cwd") {
        Some(cwd) => {
            let cwd = crate::platform::home::canonical_here(std::path::Path::new(&cwd));
            if !cwd.is_absolute() {
                return Err("cwd must be absolute or start with ~");
            }
            Ok(vec![cwd])
        }
        None => {
            let mut roots: Vec<PathBuf> = Vec::new();
            for project in host.remembered.recent() {
                if !roots.contains(&project.cwd) {
                    roots.push(project.cwd);
                }
            }
            Ok(roots)
        }
    }
}

/// `GET /sessions/:session`: one session's row. A hosted session answers its
/// [`ListedSession`](crate::protocol::sessions::ListedSession); otherwise the dormant row
/// for the id under `cwd` or the
/// remembered roots (the same roots, and the same row, as
/// `GET /sessions?state=archived`) as a [`DormantSession`], and `404` when neither
/// holds it. An id kept under several roots answers the most recently active one.
pub(crate) async fn read_session(conn: Conn) -> Conn {
    // `/sessions/:session/` is the session's web shell, which the router also
    // offers this route; it belongs to the wildcard below.
    if conn.path().ends_with('/') {
        return conn;
    }
    let Some(host) = host_of(&conn) else {
        return missing_host(conn);
    };
    let id = conn.param("session").unwrap_or_default().to_string();
    if let Some(row) = host.listed(&id) {
        return match serde_json::to_string(&row) {
            Ok(body) => conn.with_status(200).with_body(body).halt(),
            Err(error) => fail(conn, ErrorCode::Internal, error.to_string()),
        };
    }
    let roots = match scan_roots(&host, conn.querystring()) {
        Ok(roots) => roots,
        Err(message) => return fail(conn, ErrorCode::BadRequest, message),
    };
    let wanted = id.clone();
    let found = tokio::task::spawn_blocking(move || {
        host.archived(&roots)
            .into_iter()
            .find(|archived| archived.row.id == wanted)
            .map(DormantSession::read)
    })
    .await;
    match found {
        Ok(Some(row)) => match serde_json::to_string(&row) {
            Ok(body) => conn.with_status(200).with_body(body).halt(),
            Err(error) => fail(conn, ErrorCode::Internal, error.to_string()),
        },
        Ok(None) => fail(
            conn,
            ErrorCode::NotFound,
            format!(
                "no session {id:?}, hosted or archived; read GET /sessions for the current list"
            ),
        ),
        Err(error) => fail(
            conn,
            ErrorCode::Internal,
            format!("the archived scan did not finish: {error}"),
        ),
    }
}

/// The first stage of the `GET /sessions` route: answers `state=archived` and
/// refuses an unknown `state`, and passes everything else — including no `state`
/// and `state=active` — to the live listing untouched.
///
/// It sits ahead of the event-stream half, so an event-stream request for
/// `archived` is answered with the JSON array rather than a stream: the archived
/// set changes only when a log does, and there is no stream of it to subscribe to.
pub(super) async fn archived_listing(conn: Conn) -> Conn {
    let query = conn.querystring().to_string();
    match http::query_value(&query, "state").as_deref() {
        None | Some("active") => conn,
        Some("archived") => {
            let Some(host) = host_of(&conn) else {
                return missing_host(conn);
            };
            let roots = match scan_roots(&host, &query) {
                Ok(roots) => roots,
                Err(message) => return fail(conn, ErrorCode::BadRequest, message),
            };
            let rows = match tokio::task::spawn_blocking(move || host.archived(&roots)).await {
                Ok(rows) => rows,
                Err(error) => {
                    return fail(
                        conn,
                        ErrorCode::Internal,
                        format!("the archived scan did not finish: {error}"),
                    );
                }
            };
            match serde_json::to_string(&rows) {
                Ok(body) => conn.with_status(200).with_body(body).halt(),
                Err(error) => fail(conn, ErrorCode::Internal, error.to_string()),
            }
        }
        Some(other) => fail(
            conn,
            ErrorCode::BadRequest,
            format!("unknown state {other:?}; GET /sessions takes state=active or state=archived"),
        ),
    }
}
