//! `/notifications`: the ambient inbox's enqueue (docs/inbox.md).

use super::*;

/// File an ambient notification in the inbox (docs/inbox.md). 202 — it waits for the
/// next triage; a client watches the `notifications` topic. The answer is `{"seq": N}`,
/// the arrival entry's seq, and `{}` for a slot notification, which has no entry yet.
pub(super) async fn notify(mut conn: Conn) -> Conn {
    let Some(shared) = shared_of(&conn) else {
        return missing_state(conn);
    };
    let body = conn.request_body_string().await.unwrap_or_default();
    match serde_json::from_str::<NotifyBody>(&body) {
        Ok(n) => {
            let body = match shared.notify(n.source, n.body, n.slot) {
                Some(seq) => format!(r#"{{"seq":{seq}}}"#),
                None => "{}".to_string(),
            };
            conn.with_status(202).with_body(body).halt()
        }
        Err(e) => fail(
            conn,
            ErrorCode::BadRequest,
            format!("bad /notifications body: {e}"),
        ),
    }
}
