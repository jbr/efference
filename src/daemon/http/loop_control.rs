//! `/loop`: the turn engine's operator dials and its two verbs (docs/control-channel.md).

use super::*;

/// Signal the running tool (docs/control-channel.md). A verb-route on the
/// loop resource because it *is* a verb — it signals, it doesn't delete
/// state.
pub(super) async fn interrupt(conn: Conn) -> Conn {
    let Some(shared) = shared_of(&conn) else {
        return missing_state(conn);
    };
    shared.interrupt();
    conn.with_status(200)
        .with_body(r#"{"status":"interrupted"}"#)
        .halt()
}

/// Hand control back with no input (docs/control-api.md): the idle loop takes
/// another turn on the wire it already has — nothing is appended, nothing is
/// projected differently. A verb-route on the loop resource beside
/// `/loop/interrupt`, for the same reason: it signals the loop rather than
/// creating or editing a resource. Declined with `409 not_continuable` when
/// the moment has no continue in it.
pub(super) async fn continue_loop(conn: Conn) -> Conn {
    let Some(shared) = shared_of(&conn) else {
        return missing_state(conn);
    };
    shared.person_acted(crate::daemon::PersonAction::Continue);
    match shared.request_continue() {
        Ok(()) => conn
            .with_status(202)
            .with_body(r#"{"status":"continuing"}"#)
            .halt(),
        Err(why) => fail(conn, ErrorCode::NotContinuable, why.message()),
    }
}

/// Fold the cold session's history into a summary (backlog: cold-session.md): 202 while
/// the session's model writes it, its progress on the `cold` topic's `fold`; `?wait`
/// answers once it has landed or failed. Declined with `409 not_foldable` when the
/// moment has no fold in it.
pub(super) async fn fold(mut conn: Conn) -> Conn {
    let Some(shared) = shared_of(&conn) else {
        return missing_state(conn);
    };
    let body = conn.request_body_string().await.unwrap_or_default();
    let body = if body.trim().is_empty() {
        FoldBody::default()
    } else {
        match serde_json::from_str::<FoldBody>(&body) {
            Ok(body) => body,
            Err(e) => {
                return fail(
                    conn,
                    ErrorCode::BadRequest,
                    format!("bad /loop/fold body: {e}"),
                );
            }
        }
    };
    let wait = wants_wait(conn.querystring());
    match shared.request_fold(body.warm) {
        Ok(()) if wait => settle_and_state(&shared, conn).await,
        Ok(()) => conn
            .with_status(202)
            .with_body(r#"{"status":"folding"}"#)
            .halt(),
        Err(why) => fail(conn, ErrorCode::NotFoldable, why.message()),
    }
}

/// The turn engine's operator dials as one resource (docs/control-channel.md):
/// `pace`, `review_level`, and the read-only `running` — named exactly as their
/// state topics, since they are that resource's fields.
pub(super) async fn read_loop(conn: Conn) -> Conn {
    let Some(shared) = shared_of(&conn) else {
        return missing_state(conn);
    };
    conn.with_status(200).with_body(shared.loop_json()).halt()
}

/// Move the dials — either or both in one call. Validated whole before
/// anything is applied, so a request that is part-bad moves nothing.
pub(super) async fn patch_loop(mut conn: Conn) -> Conn {
    let Some(shared) = shared_of(&conn) else {
        return missing_state(conn);
    };
    let body = conn.request_body_string().await.unwrap_or_default();
    let patch = match serde_json::from_str::<LoopPatchBody>(&body) {
        Ok(p) => p,
        Err(e) => {
            return fail(conn, ErrorCode::BadRequest, format!("bad /loop body: {e}"));
        }
    };
    let level = match patch.review_level.as_deref() {
        Some(level) => match crate::policy::ReviewLevel::parse(level) {
            Some(level) => Some(level),
            // An out-of-range value on a well-formed body — `BadRequest`
            // rather than `InvalidDecision`, which is reserved for a
            // *decision* verb the daemon doesn't implement (errors.rs).
            None => {
                return fail(
                    conn,
                    ErrorCode::BadRequest,
                    "review_level must be autonomous | upstream | spotter_human | human_only",
                );
            }
        },
        None => None,
    };
    if let Some(pace) = patch.pace {
        shared.set_pace(pace.speed_ms.map(Duration::from_millis), pace.held);
    }
    if let Some(level) = level {
        shared.set_review_level(level);
    }
    conn.with_status(200).with_body(shared.loop_json()).halt()
}
