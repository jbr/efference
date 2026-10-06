//! Tests for `cold.rs` and the cold gate on the write routes (backlog: cold-session.md).

use super::*;

/// A session whose cache is already believed cold: a zero lifetime and one turn on
/// the record, a moment ago.
async fn cold_session() -> (Arc<Session>, mpsc::UnboundedReceiver<SubmittedTurn>) {
    let (shared, rx) = Session::new(std::env::current_dir().unwrap(), Log::in_memory());
    shared.set_cache_ttl(Duration::ZERO);
    shared.record_request(
        RequestOrigin::Turn,
        crate::api::RequestUsage {
            id: None,
            model: None,
            usage: crate::api::Usage::default(),
        },
    );
    tokio::time::sleep(Duration::from_millis(5)).await;
    assert!(shared.cold_since().is_some());
    (shared, rx)
}

/// A cold session refuses a turn without consent — `409 cold` with the topic in
/// `detail`, nothing queued — and takes one that carries it.
#[tokio::test]
async fn a_turn_on_a_cold_session_needs_consent() {
    let (shared, _rx) = cold_session().await;
    let server = test_server(&shared).await;

    let conn = server
        .post("/v1/turns")
        .with_body(r#"{"text":"hello"}"#)
        .await;
    conn.assert_status(409);
    conn.assert_json_body_with(|body: &Value| {
        assert_eq!(body["code"], "cold");
        assert!(body["detail"]["since"].is_u64());
    });
    assert!(shared.queue.lock().unwrap().is_empty(), "nothing queued");

    let conn = server
        .post("/v1/turns")
        .with_body(r#"{"text":"hello","accept_cold_read":true}"#)
        .await;
    conn.assert_status(202);
    assert!(shared.cold_consent.load(Ordering::SeqCst));
}

/// The loop's cold check parks a cold session — a decision point, so `?wait`
/// answers — and a granted consent releases it, consumed.
#[tokio::test]
async fn the_cold_check_parks_until_consent() {
    let (shared, _rx) = cold_session().await;
    let check = tokio::spawn({
        let shared = Arc::clone(&shared);
        async move { shared.await_warm_or_consent().await }
    });
    tokio::time::timeout(Duration::from_secs(2), async {
        while !shared.cold_parked() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("parks");
    assert!(shared.is_settled());

    shared.grant_cold_read();
    tokio::time::timeout(Duration::from_secs(2), check)
        .await
        .expect("released")
        .unwrap();
    assert!(!shared.cold_parked());
    assert!(!shared.cold_consent.load(Ordering::SeqCst), "consumed");
}

/// Consent is recorded only while the session is cold: a warm session (no request
/// on the record yet) leaves nothing for a later cold moment to consume.
#[tokio::test]
async fn consent_on_a_warm_session_is_not_kept() {
    let (shared, _rx) = Session::new(std::env::current_dir().unwrap(), Log::in_memory());
    shared.set_cache_ttl(Duration::ZERO);
    shared.grant_cold_read();
    assert!(!shared.cold_consent.load(Ordering::SeqCst));
}

/// The client surfaces `409 cold` as a [`ColdRefusal`](crate::client::ColdRefusal) —
/// an error a turn verb's caller downcasts, an outcome on continue — and its
/// `accept_cold_read` option and flag reach the route.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_client_names_a_cold_refusal_and_carries_consent() {
    let (shared, _rx) = cold_session().await;
    let client = test_client(&shared).await;

    let err = client.turn("hello").await.unwrap_err();
    let refusal = err
        .downcast_ref::<crate::client::ColdRefusal>()
        .expect("a typed refusal");
    assert!(refusal.cold["since"].is_u64());
    assert!(matches!(
        client.continue_loop(false).await.unwrap(),
        crate::client::ContinueOutcome::Cold(_)
    ));

    client
        .submit(&SubmittedTurn::new("hello", "s1").accepting_cold_read(true))
        .await
        .unwrap();
    assert!(shared.cold_consent.load(Ordering::SeqCst));
}
