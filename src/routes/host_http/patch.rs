//! `PATCH /sessions/:session`: a session's state (docs/control-api.md).
//!
//! The session resource has a location whether or not it is running, so reviving
//! one is a change to that resource rather than a verb on a path of its own.
//! [`SessionState`] is the lifecycle, kept distinct from the `status` a listing
//! reports: that is what a *live* session is doing (`running`, `idle`, `gated`,
//! `asking`, `stalled`), while this is whether it is here at all.
//!
//! `active` reopens a dormant session under its own id — [`Host::resume`], which
//! is the second boot entry behind it. `archived` is what `DELETE` already did, so
//! the two share one state word and one transition rather than describing one
//! thing two ways.
//!
//! `supervisor` is the other field a patch changes: which session supervises this
//! one (`supervise.rs`, docs/permissions.md). It is a separate patch from `state`.

use super::*;
use crate::host::SupervisorRefused;
use crate::host::resume::{Resume, ResumeRefused};
use crate::protocol::sessions::{PatchSession, SessionResumed, SessionState, SessionSupervised};

/// `PATCH /sessions/:session`: change one session's state. Mounted on every host
/// listener beside `DELETE /sessions/:session`, and registered ahead of the
/// `/sessions/:session/*` wildcard so the session is not resolved into conn state
/// first — a patch that *makes* a session live must not require it to be.
pub(crate) async fn patch_session(mut conn: Conn) -> Conn {
    let Some(host) = host_of(&conn) else {
        return missing_host(conn);
    };
    let id = conn.param("session").unwrap_or_default().to_string();
    let body = conn.request_body_string().await.unwrap_or_default();
    let body: PatchSession = match serde_json::from_str(&body) {
        Ok(body) => body,
        Err(error) => {
            return fail(
                conn,
                ErrorCode::BadRequest,
                format!("invalid body: {error}"),
            );
        }
    };
    let state = match (body.state, body.supervisor.clone()) {
        (Some(_), Some(_)) => {
            return fail(
                conn,
                ErrorCode::BadRequest,
                "`state` and `supervisor` are separate patches",
            );
        }
        (None, Some(supervisor)) => return supervise(conn, &host, &id, supervisor),
        (None, None) => {
            return fail(
                conn,
                ErrorCode::BadRequest,
                "nothing to patch: `state` and `supervisor` are the fields this route changes",
            );
        }
        (Some(state), None) => state,
    };
    match state {
        SessionState::Active => match host.resume(&id, body) {
            // Both outcomes are the state the caller asked for, so both answer
            // 200; `resumed` is what tells a repeat apart from the change.
            Ok(Resume::Starting) => active(conn, &id, true),
            Err(ResumeRefused::AlreadyActive) => active(conn, &id, false),
            Err(ResumeRefused::Conflict(reason)) => fail(conn, ErrorCode::StateMismatch, reason),
            Err(ResumeRefused::Refused(reason)) => fail(conn, ErrorCode::BadRequest, reason),
        },
        // Archiving is what `DELETE` does, spelled as the state change it is. It
        // reads the session the host is already running, so the 404 is the
        // not-hosted one rather than a bad request.
        SessionState::Archived => match host.end_by(&id, body.source) {
            Some(completion) => {
                let mut answer = completion.await.answer(&id);
                answer.resumed = Some(false);
                views::answer(conn, 200, answer)
            }
            None => no_such_session(conn, &id),
        },
    }
}

/// A `supervisor` patch (docs/permissions.md): `200 {"id", "state": "active",
/// "supervisor"}` with the pointer as it now reads, `404` for a session that is not
/// hosted, `400` naming why the change was refused.
fn supervise(conn: Conn, host: &Host, id: &str, supervisor: Option<String>) -> Conn {
    match host.set_supervisor(id, supervisor) {
        Ok(supervisor) => views::answer(
            conn,
            200,
            SessionSupervised {
                id: id.to_string(),
                state: SessionState::Active,
                supervisor,
            },
        ),
        Err(SupervisorRefused::NotHosted(_)) => no_such_session(conn, id),
        Err(SupervisorRefused::Refused(reason)) => fail(conn, ErrorCode::BadRequest, reason),
    }
}

/// The `200` an `active` patch answers with, saying whether this call is what
/// made the session live.
fn active(conn: Conn, id: &str, resumed: bool) -> Conn {
    views::answer(
        conn,
        200,
        SessionResumed {
            id: id.to_string(),
            state: SessionState::Active,
            resumed,
        },
    )
}
