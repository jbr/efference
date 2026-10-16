//! `GET /sessions?state=archived`: the sessions on disk that nothing here is
//! running (docs/control-api.md).
//!
//! The same collection as the live listing, filtered by the lifecycle word
//! [`SessionState`] already names. The rows come from [`crate::inventory`]'s
//! filesystem scan, held on the [`Host`] so an unchanged log is not read again.
//! The row is its own type: [`ListedSession`] is also the `sessions` tool's
//! schema, and an archived row has no status or processes to report.

use super::*;
use crate::inventory::{InventoryRow, sort_rows};
use std::collections::HashSet;

/// One archived session as `GET /sessions?state=archived` lists it: the
/// inventory's row, with `state` saying which collection it is from.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct ArchivedSession {
    #[serde(flatten)]
    pub row: InventoryRow,
    pub state: SessionState,
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
                row,
                state: SessionState::Archived,
            })
            .collect()
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
    match crate::daemon::http::query_value(&query, "state").as_deref() {
        None | Some("active") => conn,
        Some("archived") => {
            let Some(host) = host_of(&conn) else {
                return missing_host(conn);
            };
            let roots = match crate::daemon::http::query_value(&query, "cwd") {
                Some(cwd) => {
                    let cwd = PathBuf::from(cwd);
                    if !cwd.is_absolute() {
                        return fail(conn, ErrorCode::BadRequest, "cwd must be absolute");
                    }
                    vec![cwd]
                }
                None => {
                    let mut roots: Vec<PathBuf> = Vec::new();
                    for project in host.remembered.recent() {
                        if !roots.contains(&project.cwd) {
                            roots.push(project.cwd);
                        }
                    }
                    roots
                }
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
