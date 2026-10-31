//! `POST /sessions`: the route that mints a session (docs/control-api.md). The host's
//! side of it, [`Host::create`], is `host/create.rs`.

use super::*;
use crate::protocol::sessions::{CreateSession, CreatedSession};

/// `POST /sessions`: reserve a session from the body and prepare it in the
/// background. Mounted on every host listener, so a network caller reaches this too.
/// `201 {"id"}` once the id is reserved; the preparation shows on the session's own
/// `/state` and `/events` as the `starting` topic (docs/control-api.md).
///
/// Preparation reports to this process's stderr and may open a browser for an MCP
/// authorization, on the host — which a network caller cannot answer, though the
/// `starting` topic names the server it waits on (docs/control-api.md).
pub(crate) async fn create_session(mut conn: Conn) -> Conn {
    let Some(host) = host_of(&conn) else {
        return missing_host(conn);
    };
    let body = conn.request_body_string().await.unwrap_or_default();
    let body: CreateSession = match serde_json::from_str(&body) {
        Ok(body) => body,
        Err(error) => {
            return fail(
                conn,
                ErrorCode::BadRequest,
                format!("invalid body: {error}"),
            );
        }
    };
    // A human root: no session commissioned it, so it has no `created_by`
    // (docs/tools.md).
    match host.create(body, crate::env::Lineage::default()) {
        Ok(starting) => views::answer(
            conn,
            201,
            CreatedSession {
                id: starting.id().to_string(),
            },
        ),
        Err(reason) => fail(conn, ErrorCode::BadRequest, reason),
    }
}
