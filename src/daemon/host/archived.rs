//! `GET /sessions?state=archived` and `GET /sessions/:session`: the sessions on disk that nothing here is
//! running (docs/control-api.md).
//!
//! The same collection as the live listing, filtered by the lifecycle word
//! [`SessionState`] already names. The rows come from [`crate::inventory`]'s
//! filesystem scan, held on the [`Host`] so an unchanged log is not read again.
//! The row is its own type: an archived row has no status or processes to report,
//! which [`ListedSession`] carries. The single read answers a [`DormantSession`],
//! which adds what reading the whole log and the usage record learns; the listing
//! reads neither.

use super::*;
use crate::inventory::{InventoryRow, sort_rows};
use crate::session_dir::SESSION_BASE;
use std::collections::HashSet;

impl DormantSession {
    /// `archived`, with what its log and usage record say. Blocking: reads the log
    /// whole, since a resume's switch can record selections anywhere in it.
    fn read(archived: ArchivedSession) -> Self {
        let dir = archived.row.cwd.join(SESSION_BASE).join(&archived.row.id);
        let selections = crate::persist::load_recording(dir.join(crate::session_dir::LOG_NAME))
            .ok()
            .map(|recording| crate::boot::recorded_selections(&recording))
            .filter(|selections| !selections.is_empty());
        let context_tokens = UsageLine::read(&dir.join(USAGE_NAME))
            .ok()
            .and_then(|lines| last_turn_usage(&lines))
            .map(|usage| usage.prompt_tokens());
        Self {
            archived,
            selections,
            context_tokens,
        }
    }
}

impl Host {
    /// The archived rows for `roots`, most recently active first, excluding
    /// sessions this host holds (same id and same working directory).
    ///
    /// Blocking: stats and reads logs. Callers off the executor.
    fn archived(&self, roots: &[PathBuf]) -> Vec<ArchivedSession> {
        let live: HashSet<(String, String)> = self
            .listing()
            .into_iter()
            .filter_map(|summary| Some((summary.id, summary.cwd?)))
            .collect();
        let mut rows: Vec<InventoryRow> = roots
            .iter()
            .flat_map(|root| self.inventory.refresh(root))
            .filter(|row| !live.contains(&(row.id.clone(), row.cwd.to_string_lossy().into_owned())))
            .collect();
        sort_rows(&mut rows);
        rows.into_iter()
            .map(|row| ArchivedSession {
                resume_failure: self.resume_failure(&row.cwd, &row.id),
                row,
                state: SessionState::Archived,
            })
            .collect()
    }
}

/// The roots an archived read covers: the `cwd` query when given, else every
/// remembered root, most recently used first.
fn scan_roots(host: &Host, query: &str) -> Result<Vec<PathBuf>, &'static str> {
    match http::query_value(query, "cwd") {
        Some(cwd) => {
            let cwd = PathBuf::from(cwd);
            if !cwd.is_absolute() {
                return Err("cwd must be absolute");
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
/// [`ListedSession`]; otherwise the dormant row for the id under `cwd` or the
/// remembered roots (the same roots, and the same row, as
/// `GET /sessions?state=archived`) as a [`DormantSession`], and `404` when neither
/// holds it. An id kept under several roots answers the most recently active one.
pub(super) async fn read_session(conn: Conn) -> Conn {
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
