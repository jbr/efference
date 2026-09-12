//! `/push`: the browser's Web Push key and subscription store (docs/web-push.md).

use super::*;

/// What a browser needs before it can subscribe (docs/web-push.md): the
/// machine's VAPID public key, which is `applicationServerKey` verbatim.
/// Under `/v1` with everything else, so the service worker's never-cache
/// rule and the TCP gate cover it without either learning a new prefix.
pub(super) async fn read_push_key(conn: Conn) -> Conn {
    let Some(root) = crate::auth::Root::installed() else {
        return fail(conn, ErrorCode::Internal, NO_ROOT);
    };
    let body = serde_json::json!({ "key": crate::push::public_key(&root.vapid_key()) });
    conn.with_status(200).with_body(body.to_string()).halt()
}

/// Record a browser subscription, in the shape `PushSubscription.toJSON()`
/// produces — so the client forwards what it was handed rather than
/// reshaping it. An upsert on the endpoint, which is what makes a repeated
/// POST from a page reload idempotent.
pub(super) async fn subscribe(mut conn: Conn) -> Conn {
    let body = conn.request_body_string().await.unwrap_or_default();
    let subscription = match serde_json::from_str::<crate::push::SubscriptionJson>(&body) {
        Ok(json) => crate::push::Subscription::from(json),
        Err(e) => {
            return fail(
                conn,
                ErrorCode::BadRequest,
                format!("bad subscription: {e}"),
            );
        }
    };
    match crate::push::Store::machine_scoped().upsert(subscription) {
        Ok(()) => conn
            .with_status(201)
            .with_body(r#"{"status":"subscribed"}"#)
            .halt(),
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
    match crate::push::Store::machine_scoped().remove(&endpoint) {
        Ok(()) => conn
            .with_status(200)
            .with_body(r#"{"status":"unsubscribed"}"#)
            .halt(),
        Err(e) => fail(
            conn,
            ErrorCode::Internal,
            format!("could not forget the subscription: {e}"),
        ),
    }
}
