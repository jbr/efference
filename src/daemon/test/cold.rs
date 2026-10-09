//! Tests for `cold.rs` and the cold gate on the write routes (backlog: cold-session.md).

use super::*;
use crate::daemon::cold::ParkReason;
use crate::log::Mark;

/// A session whose cache is already believed cold: a zero lifetime and one turn on
/// the record, a moment ago.
async fn cold_session() -> (Arc<Session>, mpsc::UnboundedReceiver<SubmittedTurn>) {
    let (shared, rx) = Session::new(std::env::current_dir().unwrap(), Log::in_memory());
    shared.set_cache_ttl(Duration::ZERO);
    append_foldable_history(&shared);
    shared.record_request(
        RequestOrigin::Turn,
        crate::api::RequestUsage::new(crate::api::Usage::default()),
    );
    tokio::time::sleep(Duration::from_millis(5)).await;
    assert!(shared.cold_since().is_some());
    (shared, rx)
}

fn no_usage() -> crate::api::RequestUsage {
    crate::api::RequestUsage::new(crate::api::Usage::default())
}

/// A peek on the agent's own client reads the main prefix, so it ends the session's
/// cold belief like a turn; a peek on a separate triage client does not.
#[test]
fn a_shared_peek_refreshes_the_cold_belief_and_an_own_peek_does_not() {
    let usage_path = std::env::temp_dir().join("efference-cold-peek-test-usage.jsonl");
    let stale = crate::log::Timestamp::now().0 - 35 * 60_000;
    std::fs::write(
        &usage_path,
        format!("{{\"at_ms\":{stale},\"origin\":\"turn\"}}\n"),
    )
    .unwrap();
    let (shared, _rx) = Session::new(std::env::current_dir().unwrap(), Log::in_memory());
    shared.set_cache_ttl(Duration::from_secs(300));
    append_foldable_history(&shared);
    shared.attach_usage_log(&usage_path);
    assert!(
        shared.cold_since().is_some(),
        "the 35-minute-old turn is stale"
    );

    shared.record_peek(false, no_usage());
    assert!(
        shared.cold_since().is_some(),
        "a peek on a separate triage client leaves the main prefix as it was"
    );
    shared.record_peek(true, no_usage());
    assert_eq!(
        shared.cold_since(),
        None,
        "a shared peek re-warmed the prefix"
    );
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

    // The park is its own status, ranked ahead of the running flag, and rides the
    // `cold` topic so a session view can tell it from a slow request.
    shared.running.store(true, Ordering::SeqCst);
    let crate::daemon::status::SessionStatus::Parked {
        reason: ParkReason::Expired { since, .. },
    } = shared.status()
    else {
        panic!("a parked loop is not running: {:?}", shared.status());
    };
    assert_eq!(Some(since), shared.cold_since());
    let state: serde_json::Value = serde_json::from_str(&shared.state_json()).unwrap();
    assert_eq!(state["park"]["reason"], "expired");
    assert_eq!(state["park"]["since"], state["cold"]["since"]);
    assert!(
        shared
            .status_body("s1", &shared.status())
            .contains("parked mid-turn")
    );
    shared.running.store(false, Ordering::SeqCst);

    shared.grant_cold_read(crate::log::ConsentSource::Turn);
    tokio::time::timeout(Duration::from_secs(2), check)
        .await
        .expect("released")
        .unwrap();
    assert!(!shared.cold_parked());
    assert_eq!(shared.status(), crate::daemon::status::SessionStatus::Idle);
    assert!(!shared.cold_consent.load(Ordering::SeqCst), "consumed");
}

/// A fold at a mid-turn park ends the cold belief but not the park: the loop waits for
/// the next turn or continue, and the `park` topic says so while `cold` is null.
#[tokio::test]
async fn a_fold_leaves_the_park_folded() {
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
    shared
        .append(1, Payload::Mark(Mark::boundary(0, "account")))
        .unwrap();
    assert_eq!(shared.topic_json(Topic::Cold), Value::Null);
    assert_eq!(
        shared.topic_json(Topic::Park),
        serde_json::json!({ "reason": "folded" })
    );
    assert_eq!(
        shared.status(),
        crate::daemon::status::SessionStatus::Parked {
            reason: ParkReason::Folded
        }
    );
    shared.wake_cold_check();
    tokio::time::timeout(Duration::from_secs(2), check)
        .await
        .expect("released")
        .unwrap();
    assert_eq!(shared.topic_json(Topic::Park), Value::Null);
}

/// Consent is recorded only while the session is cold: a warm session (no request
/// on the record yet) leaves nothing for a later cold moment to consume.
#[tokio::test]
async fn consent_on_a_warm_session_is_not_kept() {
    let (shared, _rx) = Session::new(std::env::current_dir().unwrap(), Log::in_memory());
    shared.set_cache_ttl(Duration::ZERO);
    shared.grant_cold_read(crate::log::ConsentSource::Turn);
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

/// A session with nothing to fold is not cold: with one answer there is no question to
/// put, so its next send goes through without consent.
#[tokio::test]
async fn nothing_to_fold_is_not_cold() {
    let (shared, _rx) = Session::new(std::env::current_dir().unwrap(), Log::in_memory());
    shared.set_cache_ttl(Duration::ZERO);
    shared
        .append(1, Payload::User(vec![ContentBlock::Text("hi".into())]))
        .unwrap();
    shared.record_request(
        RequestOrigin::Turn,
        crate::api::RequestUsage::new(crate::api::Usage::default()),
    );
    tokio::time::sleep(Duration::from_millis(5)).await;
    assert_eq!(shared.cold_since(), None);
}

/// The `Cold` records in the log, in order.
fn cold_records(shared: &Session) -> Vec<crate::log::ColdEvent> {
    shared
        .log
        .lock()
        .unwrap()
        .entries()
        .iter()
        .filter_map(|e| match &e.payload {
            Payload::Cold(event) => Some(event.clone()),
            _ => None,
        })
        .collect()
}

/// An approval that carries `accept_cold_read` while the session is cold grants the
/// consent before the decision reaches the loop, so the cold check after the approved call
/// does not park (docs/control-api.md). The log records the consent and no park.
#[tokio::test]
async fn an_approval_with_the_flag_consents_and_no_park_follows() {
    let (shared, _rx) = cold_session().await;
    let server = test_server(&shared).await;
    let call = bash_call("cargo build");
    let _gate = shared.register_pending(&call, None);

    let conn = server
        .put(&format!("/v1/approvals/{}", call.id.0))
        .with_body(r#"{"decision":"approve","accept_cold_read":true}"#)
        .await;
    conn.assert_status(200);
    assert!(shared.cold_consent.load(Ordering::SeqCst));

    tokio::time::timeout(Duration::from_secs(2), shared.await_warm_or_consent())
        .await
        .expect("the consent releases the check at once");
    assert!(!shared.cold_parked());
    assert_eq!(
        cold_records(&shared),
        vec![
            crate::log::ColdEvent::Consented {
                source: crate::log::ConsentSource::Approval
            },
            // Nothing parked, so nothing unparked.
        ]
    );
}

/// An approval without the flag behaves as it always did: the call runs and the loop
/// then parks at the cold check, which the log records.
#[tokio::test]
async fn an_approval_without_the_flag_still_parks() {
    let (shared, _rx) = cold_session().await;
    let server = test_server(&shared).await;
    let call = bash_call("cargo build");
    let _gate = shared.register_pending(&call, None);

    let conn = server
        .put(&format!("/v1/approvals/{}", call.id.0))
        .with_body(r#"{"decision":"approve"}"#)
        .await;
    conn.assert_status(200);
    assert!(!shared.cold_consent.load(Ordering::SeqCst));

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

    // The second question gets its answer; the log reads park, consent, resume.
    shared.grant_cold_read(crate::log::ConsentSource::Continue);
    tokio::time::timeout(Duration::from_secs(2), check)
        .await
        .expect("released")
        .unwrap();
    let records = cold_records(&shared);
    assert!(matches!(
        records[0],
        crate::log::ColdEvent::Parked {
            reason: crate::log::ColdReason::Expired,
            ..
        }
    ));
    assert_eq!(
        records[1..],
        [
            crate::log::ColdEvent::Consented {
                source: crate::log::ConsentSource::Continue
            },
            crate::log::ColdEvent::Unparked {
                cause: crate::log::UnparkCause::Consent
            },
        ]
    );
}

/// The flag on a warm session records nothing, and `abort` ignores it; a redirect, which
/// lets the turn go on, honors it.
#[tokio::test]
async fn the_flag_is_recorded_only_while_cold_and_not_for_an_abort() {
    let (warm, _rx) = Session::new(std::env::current_dir().unwrap(), Log::in_memory());
    warm.set_cache_ttl(Duration::from_secs(300));
    let server = test_server(&warm).await;
    let call = bash_call("ls");
    let _gate = warm.register_pending(&call, None);
    server
        .put(&format!("/v1/approvals/{}", call.id.0))
        .with_body(r#"{"decision":"approve","accept_cold_read":true}"#)
        .await
        .assert_status(200);
    assert!(!warm.cold_consent.load(Ordering::SeqCst));
    assert!(cold_records(&warm).is_empty());

    let (shared, _rx) = cold_session().await;
    let server = test_server(&shared).await;
    let call = bash_call("ls");
    let _gate = shared.register_pending(&call, None);
    server
        .put(&format!("/v1/approvals/{}", call.id.0))
        .with_body(r#"{"decision":"abort","accept_cold_read":true}"#)
        .await
        .assert_status(200);
    assert!(!shared.cold_consent.load(Ordering::SeqCst), "abort");

    let call = bash_call("pwd");
    let _gate = shared.register_pending(&call, None);
    server
        .put(&format!("/v1/approvals/{}", call.id.0))
        .with_body(r#"{"decision":"redirect","message":"no","accept_cold_read":true}"#)
        .await
        .assert_status(200);
    assert!(shared.cold_consent.load(Ordering::SeqCst), "redirect");
}

/// A decision on a gate that has moved on grants nothing.
#[tokio::test]
async fn a_moved_gate_grants_no_consent() {
    let (shared, _rx) = cold_session().await;
    let server = test_server(&shared).await;
    server
        .put("/v1/approvals/toolu_gone")
        .with_body(r#"{"decision":"approve","accept_cold_read":true}"#)
        .await
        .assert_status(409);
    assert!(!shared.cold_consent.load(Ordering::SeqCst));
    assert!(cold_records(&shared).is_empty());
}

/// An answer to an `ask` carries the consent the same way a turn does, recorded as an
/// answer; the turn route already took the flag, and the source tells the two apart.
#[tokio::test]
async fn an_answer_to_an_ask_with_the_flag_consents() {
    let (shared, _rx) = cold_session().await;
    let server = test_server(&shared).await;
    let mut wait = shared.begin_ask("ask_1", "Proceed?".into(), 1, 1);

    server
        .post("/v1/turns")
        .with_body(r#"{"text":"yes"}"#)
        .await
        .assert_status(409);
    server
        .post("/v1/turns")
        .with_body(r#"{"text":"yes","accept_cold_read":true}"#)
        .await
        .assert_status(202);
    assert_eq!(wait.reply().await.unwrap().text, "yes");
    assert!(shared.cold_consent.load(Ordering::SeqCst));
    assert_eq!(
        cold_records(&shared),
        vec![crate::log::ColdEvent::Consented {
            source: crate::log::ConsentSource::Ask
        }]
    );
}
