//! `/notifications`: the ambient inbox's enqueue and dismiss (docs/inbox.md).

use super::*;

/// Enqueue an ambient notification into the inbox (docs/inbox.md). 202 — it waits
/// for the next triage; a client watches the `notifications` topic.
pub(super) async fn notify(mut conn: Conn) -> Conn {
    let Some(shared) = shared_of(&conn) else {
        return missing_state(conn);
    };
    let body = conn.request_body_string().await.unwrap_or_default();
    match serde_json::from_str::<NotifyBody>(&body) {
        Ok(n) => {
            let id = shared.notify(n.source, n.body, n.slot, n.replaces);
            conn.with_status(202)
                .with_body(format!(r#"{{"id":{id}}}"#))
                .halt()
        }
        Err(e) => fail(
            conn,
            ErrorCode::BadRequest,
            format!("bad /notifications body: {e}"),
        ),
    }
}

/// Dismiss a pending notification out-of-band by its queue-local id (docs/inbox.md).
pub(super) async fn dismiss_notification(conn: Conn) -> Conn {
    let Some(shared) = shared_of(&conn) else {
        return missing_state(conn);
    };
    match conn.param("id").and_then(|s| s.parse::<u64>().ok()) {
        Some(id) if shared.dismiss_notification(id) => conn
            .with_status(200)
            .with_body(r#"{"status":"dismissed"}"#)
            .halt(),
        Some(_) => fail(
            conn,
            ErrorCode::NotFound,
            "no pending notification with that id",
        ),
        None => fail(
            conn,
            ErrorCode::BadRequest,
            "notification id must be an integer",
        ),
    }
}
