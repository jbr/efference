//! `/loop`: the turn engine's operator dials and its two verbs (docs/control-channel.md).

use super::*;
use crate::protocol::answers::{StatusBody, StatusWord};

/// Signal the running tool (docs/control-channel.md). A verb-route on the
/// loop resource because it *is* a verb — it signals, it doesn't delete
/// state.
pub(super) async fn interrupt(mut conn: Conn) -> Conn {
    let Some(shared) = shared_of(&conn) else {
        return missing_state(conn);
    };
    // The body is optional: a client that says nothing about itself interrupts
    // as the operator, with no control named (docs/control-api.md).
    let body = conn.request_body_string().await.unwrap_or_default();
    let origin = if body.trim().is_empty() {
        crate::record::log::InterruptOrigin::default()
    } else {
        match serde_json::from_str::<crate::record::log::InterruptOrigin>(&body) {
            Ok(origin) => origin,
            Err(e) => {
                return fail(
                    conn,
                    ErrorCode::BadRequest,
                    format!("bad /loop/interrupt body: {e}"),
                );
            }
        }
    };
    shared.interrupt(origin);
    views::answer(conn, 200, StatusBody::new(StatusWord::Interrupted))
}

/// Hand control back with no input (docs/control-api.md): the idle loop takes
/// another turn on the wire it already has — nothing is appended, nothing is
/// projected differently. A verb-route on the loop resource beside
/// `/loop/interrupt`, for the same reason: it signals the loop rather than
/// creating or editing a resource. Declined with `409 not_continuable` when
/// the moment has no continue in it.
pub(super) async fn continue_loop(mut conn: Conn) -> Conn {
    let Some(shared) = shared_of(&conn) else {
        return missing_state(conn);
    };
    // The body is optional: a client that says nothing about itself continues as the
    // operator (docs/control-api.md).
    let body = conn.request_body_string().await.unwrap_or_default();
    let origin = if body.trim().is_empty() {
        crate::record::log::ContinueOrigin::default()
    } else {
        match serde_json::from_str::<crate::record::log::ContinueOrigin>(&body) {
            Ok(origin) => origin,
            Err(e) => {
                return fail(
                    conn,
                    ErrorCode::BadRequest,
                    format!("bad /loop/continue body: {e}"),
                );
            }
        }
    };
    match shared.request_continue(origin) {
        Ok(()) => views::answer(conn, 202, StatusBody::new(StatusWord::Continuing)),
        Err(why) => fail(conn, ErrorCode::NotContinuable, why.message()),
    }
}

/// Compact the session's history into a summary (docs/summary.md): 202 while the
/// session's model writes it, its progress in `current_operations`; `?wait` answers once
/// it has landed or failed. Declined with `409 not_compactable` when the moment has no
/// compaction in it ([`Session::compaction_verdict`], the `verbs` topic's answer).
pub(super) async fn compaction(mut conn: Conn) -> Conn {
    let Some(shared) = shared_of(&conn) else {
        return missing_state(conn);
    };
    let body = conn.request_body_string().await.unwrap_or_default();
    let body = if body.trim().is_empty() {
        CompactBody::default()
    } else {
        match serde_json::from_str::<CompactBody>(&body) {
            Ok(body) => body,
            Err(e) => {
                return fail(
                    conn,
                    ErrorCode::BadRequest,
                    format!("bad /loop/compact body: {e}"),
                );
            }
        }
    };
    let wait = wants_wait(conn.querystring());
    match shared.request_compaction(body.warm) {
        Ok(()) if wait => {
            let _ = tokio::time::timeout(SETTLE_TIMEOUT, shared.await_compaction()).await;
            conn.with_status(200).with_body(shared.state_json()).halt()
        }
        Ok(()) => views::answer(conn, 202, StatusBody::new(StatusWord::Compacting)),
        Err(why) => fail(conn, ErrorCode::NotCompactable, why.message()),
    }
}

/// The turn engine's operator dials as one resource (docs/control-channel.md):
/// `pace`, `review_level`, and the read-only `loop_phase` — named exactly as their
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
                    "review_level must be autonomous | supervisor | spotter_human | human_only",
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
