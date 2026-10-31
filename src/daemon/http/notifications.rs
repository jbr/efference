//! `/notifications`: the ambient inbox's enqueue (docs/inbox.md).

use super::*;
use crate::protocol::answers::FiledNotification;

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
            let seq = shared.notify(n.source, n.body, n.slot);
            views::answer(conn, 202, FiledNotification { seq })
        }
        Err(e) => fail(
            conn,
            ErrorCode::BadRequest,
            format!("bad /notifications body: {e}"),
        ),
    }
}
