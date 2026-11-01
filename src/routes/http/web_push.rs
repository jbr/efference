//! `/push`: the browser's Web Push key and subscription store (docs/web-push.md).

use super::*;
use crate::protocol::answers::{PushKey, StatusBody, StatusWord};

/// What a browser needs before it can subscribe (docs/web-push.md): the
/// machine's VAPID public key, which is `applicationServerKey` verbatim.
/// Under `/v1` with everything else, so the service worker's never-cache
/// rule and the TCP gate cover it without either learning a new prefix.
pub(super) async fn read_push_key(conn: Conn) -> Conn {
    let Some(root) = crate::platform::auth::Root::installed() else {
        return fail(conn, ErrorCode::Internal, NO_ROOT);
    };
    let key = crate::platform::push::public_key(&root.vapid_key());
    views::answer(conn, 200, PushKey { key })
}

/// Record a browser subscription, in the shape `PushSubscription.toJSON()`
/// produces — so the client forwards what it was handed rather than
/// reshaping it. An upsert on the endpoint, which is what makes a repeated
/// POST from a page reload idempotent.
pub(super) async fn subscribe(mut conn: Conn) -> Conn {
    let body = conn.request_body_string().await.unwrap_or_default();
    let subscription = match serde_json::from_str::<crate::platform::push::SubscriptionJson>(&body)
    {
        Ok(json) => crate::platform::push::Subscription::from(json),
        Err(e) => {
            return fail(
                conn,
                ErrorCode::BadRequest,
                format!("bad subscription: {e}"),
            );
        }
    };
    match crate::platform::push::Store::machine_scoped().upsert(&subscription) {
        Ok(()) => views::answer(conn, 201, StatusBody::new(StatusWord::Subscribed)),
        Err(e) => fail(
            conn,
            ErrorCode::Internal,
            format!("could not record the subscription: {e}"),
        ),
    }
}

/// Forget one subscription, named by its endpoint. Always 200: an
/// unsubscribe is a statement about the end state, and a browser that
/// unsubscribes twice is not in error. The endpoint rides in a body rather
/// than the path because it is an absolute URL.
pub(super) async fn unsubscribe(mut conn: Conn) -> Conn {
    let body = conn.request_body_string().await.unwrap_or_default();
    let endpoint = match serde_json::from_str::<PushUnsubscribeBody>(&body) {
        Ok(b) => b.endpoint,
        Err(e) => {
            return fail(conn, ErrorCode::BadRequest, format!("bad unsubscribe: {e}"));
        }
    };
    match crate::platform::push::Store::machine_scoped().remove(&endpoint) {
        Ok(()) => views::answer(conn, 200, StatusBody::new(StatusWord::Unsubscribed)),
        Err(e) => fail(
            conn,
            ErrorCode::Internal,
            format!("could not forget the subscription: {e}"),
        ),
    }
}
