//! Tests for `cold.rs` and the person actions on the write routes (backlog: cold-session.md).

use super::*;
use crate::daemon::cold::ParkReason;
use crate::log::Summary;
use crate::log::Telemetry;

/// A session whose cache is already believed cold: a zero lifetime and one turn on
/// the record, a moment ago.
async fn cold_session() -> (Arc<Session>, mpsc::UnboundedReceiver<SubmittedTurn>) {
    let (shared, rx) = Session::new(std::env::current_dir().unwrap(), Log::in_memory());
    shared.set_cache_ttl(Duration::ZERO);
    append_compactable_history(&shared);
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

/// A triage on the agent's own client reads the main prefix, so it ends the session's
/// cold belief like a turn; a triage on a separate triage client does not.
#[test]
fn a_shared_triage_refreshes_the_cold_belief_and_an_own_triage_does_not() {
    let usage_path = std::env::temp_dir().join("efference-cold-triage-test-usage.jsonl");
    let stale = crate::log::Timestamp::now().0 - 35 * 60_000;
    std::fs::write(
        &usage_path,
        format!("{{\"at_ms\":{stale},\"origin\":\"turn\"}}\n"),
    )
    .unwrap();
    let (shared, _rx) = Session::new(std::env::current_dir().unwrap(), Log::in_memory());
    shared.set_cache_ttl(Duration::from_secs(300));
    append_compactable_history(&shared);
    shared.attach_usage_log(&usage_path);
    assert!(
        shared.cold_since().is_some(),
        "the 35-minute-old turn is stale"
    );

    shared.record_triage(false, no_usage());
    assert!(
        shared.cold_since().is_some(),
        "a triage on a separate triage client leaves the main prefix as it was"
    );
    shared.record_triage(true, no_usage());
    assert_eq!(
        shared.cold_since(),
        None,
        "a shared triage re-warmed the prefix"
    );
}

/// A turn on a cold session is accepted like any other: no `409`, queued, and the
/// person's action is noted for the loop's cold check.
#[tokio::test]
async fn a_turn_on_a_cold_session_goes_through() {
    let (shared, _rx) = cold_session().await;
    let server = test_server(&shared).await;

    let conn = server
        .post("/v1/turns")
        .with_body(r#"{"text":"hello"}"#)
        .await;
    conn.assert_status(202);
    assert_eq!(shared.queue.lock().unwrap().len(), 1, "queued");
    assert!(shared.person_acted.load(Ordering::SeqCst));
    assert_eq!(
        cold_records(&shared),
        vec![crate::log::ColdEvent::Released {
            by: crate::log::ReleasedBy::Turn
        }]
    );
}

/// The loop's cold check parks a cold session — a decision point, so `?wait`
/// answers — and a person's action releases it, consumed.
#[tokio::test]
async fn the_cold_check_parks_until_a_person_acts() {
    let (shared, _rx) = cold_session().await;
    let check = tokio::spawn({
        let shared = Arc::clone(&shared);
        async move { shared.await_warm_or_person().await }
    });
    tokio::time::timeout(Duration::from_secs(2), async {
        while shared.loop_phase() != crate::daemon::LoopPhase::ColdParked {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("parks");
    assert!(shared.is_settled());

    // The park is its own phase and status, and rides the `cold` topic so a session
    // view can tell it from a slow request.
    let crate::daemon::status::SessionStatus::Parked {
        reason: ParkReason::Expired { since, .. },
    } = shared.status()
    else {
        panic!("a parked loop is not running: {:?}", shared.status());
    };
    assert_eq!(Some(since), shared.cold_since());
    let state: serde_json::Value = serde_json::from_str(&shared.state_json()).unwrap();
    assert_eq!(state["loop_phase"]["phase"], "cold_parked");
    assert_eq!(state["cold"]["since"], serde_json::json!(since));
    assert_eq!(state["status"]["condition"]["kind"], "parked");

    shared.release_cold(crate::log::ReleasedBy::Turn);
    assert_eq!(
        shared.loop_phase(),
        crate::daemon::LoopPhase::Requesting,
        "the person's action ends the park before the loop wakes"
    );
    assert!(!shared.is_settled());
    tokio::time::timeout(Duration::from_secs(2), check)
        .await
        .expect("released")
        .unwrap();
    assert_eq!(shared.loop_phase(), crate::daemon::LoopPhase::Requesting);
    assert_eq!(
        shared.status(),
        crate::daemon::status::SessionStatus::Running
    );
    assert!(!shared.person_acted.load(Ordering::SeqCst), "consumed");
}

/// A compaction at a mid-turn park ends the cold belief but not the park: the loop waits for
/// the next turn or continue, and the phase says so while `cold` is null.
#[tokio::test]
async fn a_fold_leaves_the_park_folded() {
    let (shared, _rx) = cold_session().await;
    let check = tokio::spawn({
        let shared = Arc::clone(&shared);
        async move { shared.await_warm_or_person().await }
    });
    tokio::time::timeout(Duration::from_secs(2), async {
        while shared.loop_phase() != crate::daemon::LoopPhase::ColdParked {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("parks");
    shared
        .append(1, Payload::Summary(Summary::consolidation(0, 0, "account")))
        .unwrap();
    assert_eq!(shared.topic_json(Topic::Cold), Value::Null);
    assert_eq!(
        shared.topic_json(Topic::LoopPhase),
        serde_json::json!({ "phase": "cold_parked" })
    );
    assert_eq!(
        shared.status(),
        crate::daemon::status::SessionStatus::Parked {
            reason: ParkReason::Compacted
        }
    );
    shared.wake_cold_check();
    tokio::time::timeout(Duration::from_secs(2), check)
        .await
        .expect("released")
        .unwrap();
    assert_eq!(
        shared.topic_json(Topic::LoopPhase),
        serde_json::json!({ "phase": "requesting" })
    );
}

/// A person's action is recorded only while the session is cold: a warm session (no
/// request on the record yet) leaves nothing for a later cold moment to consume.
#[tokio::test]
async fn a_person_acting_on_a_warm_session_is_not_kept() {
    let (shared, _rx) = Session::new(std::env::current_dir().unwrap(), Log::in_memory());
    shared.set_cache_ttl(Duration::ZERO);
    shared.release_cold(crate::log::ReleasedBy::Turn);
    assert!(!shared.person_acted.load(Ordering::SeqCst));
    assert!(cold_records(&shared).is_empty());
}

/// The client's turn and continue verbs take a cold session's request as they would a
/// warm one's.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_client_sends_to_a_cold_session_without_a_flag() {
    let (shared, _rx) = cold_session().await;
    let client = test_client(&shared).await;

    client.turn("hello").await.unwrap();
    assert!(shared.person_acted.load(Ordering::SeqCst));
}

/// A session with nothing to compact is not cold: with one answer there is no question to
/// put, so its next send is not a cold read.
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
            Payload::Telemetry(Telemetry::Cold(event)) => Some(event.clone()),
            _ => None,
        })
        .collect()
}

/// An approval while the session is cold is a person's action noted before the decision
/// reaches the loop, so the cold check after the approved call does not park
/// (docs/control-api.md). The log records the release and no park.
#[tokio::test]
async fn an_approval_releases_the_next_check_and_no_park_follows() {
    let (shared, _rx) = cold_session().await;
    let server = test_server(&shared).await;
    let call = bash_call("cargo build");
    let _gate = shared.register_pending(&call, None);

    let conn = server
        .put(&format!("/v1/approvals/{}", call.id.0))
        .with_body(r#"{"decision":"approve"}"#)
        .await;
    conn.assert_status(200);
    assert!(shared.person_acted.load(Ordering::SeqCst));

    tokio::time::timeout(Duration::from_secs(2), shared.await_warm_or_person())
        .await
        .expect("the approval releases the check at once");
    assert!(shared.loop_phase() != crate::daemon::LoopPhase::ColdParked);
    assert_eq!(
        cold_records(&shared),
        vec![
            crate::log::ColdEvent::Released {
                by: crate::log::ReleasedBy::Approval
            },
            // Nothing parked, so nothing unparked.
        ]
    );
}

/// Each person action releases a parked loop and is recorded as such: the log reads
/// park, release, resume.
#[tokio::test]
async fn every_person_action_releases_a_park_and_is_recorded() {
    use crate::log::ReleasedBy;
    for by in [
        ReleasedBy::Turn,
        ReleasedBy::Approval,
        ReleasedBy::Ask,
        ReleasedBy::Continue,
    ] {
        let (shared, _rx) = cold_session().await;
        let check = tokio::spawn({
            let shared = Arc::clone(&shared);
            async move { shared.await_warm_or_person().await }
        });
        tokio::time::timeout(Duration::from_secs(2), async {
            while shared.loop_phase() != crate::daemon::LoopPhase::ColdParked {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("parks");

        shared.release_cold(by);
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
                crate::log::ColdEvent::Released { by },
                crate::log::ColdEvent::Unparked {
                    cause: crate::log::UnparkCause::Person
                },
            ]
        );
    }
}

/// An abort lets no further request go, so it is not a release; a redirect, which lets
/// the turn go on, is. A decision on a warm session records nothing.
#[tokio::test]
async fn an_abort_does_not_release_and_a_warm_decision_records_nothing() {
    let (warm, _rx) = Session::new(std::env::current_dir().unwrap(), Log::in_memory());
    warm.set_cache_ttl(Duration::from_secs(300));
    let server = test_server(&warm).await;
    let call = bash_call("ls");
    let _gate = warm.register_pending(&call, None);
    server
        .put(&format!("/v1/approvals/{}", call.id.0))
        .with_body(r#"{"decision":"approve"}"#)
        .await
        .assert_status(200);
    assert!(!warm.person_acted.load(Ordering::SeqCst));
    assert!(cold_records(&warm).is_empty());

    let (shared, _rx) = cold_session().await;
    let server = test_server(&shared).await;
    let call = bash_call("ls");
    let _gate = shared.register_pending(&call, None);
    server
        .put(&format!("/v1/approvals/{}", call.id.0))
        .with_body(r#"{"decision":"abort"}"#)
        .await
        .assert_status(200);
    assert!(!shared.person_acted.load(Ordering::SeqCst), "abort");

    let call = bash_call("pwd");
    let _gate = shared.register_pending(&call, None);
    server
        .put(&format!("/v1/approvals/{}", call.id.0))
        .with_body(r#"{"decision":"redirect","message":"no"}"#)
        .await
        .assert_status(200);
    assert!(shared.person_acted.load(Ordering::SeqCst), "redirect");
}

/// A decision on a gate that has moved on releases nothing.
#[tokio::test]
async fn a_moved_gate_releases_nothing() {
    let (shared, _rx) = cold_session().await;
    let server = test_server(&shared).await;
    server
        .put("/v1/approvals/toolu_gone")
        .with_body(r#"{"decision":"approve"}"#)
        .await
        .assert_status(409);
    assert!(!shared.person_acted.load(Ordering::SeqCst));
    assert!(cold_records(&shared).is_empty());
}

/// An answer to an `ask` is a turn on the route and is recorded as an answer.
#[tokio::test]
async fn an_answer_to_an_ask_is_recorded_as_an_answer() {
    let (shared, _rx) = cold_session().await;
    let server = test_server(&shared).await;
    let mut wait = shared.begin_ask("ask_1", "Proceed?".into(), 1, 1);

    server
        .post("/v1/turns")
        .with_body(r#"{"text":"yes","answers":"ask_1"}"#)
        .await
        .assert_status(202);
    assert_eq!(wait.reply().await.unwrap().text, "yes");
    assert!(shared.person_acted.load(Ordering::SeqCst));
    assert_eq!(
        cold_records(&shared),
        vec![crate::log::ColdEvent::Released {
            by: crate::log::ReleasedBy::Ask
        }]
    );
}

/// An idle inbox wake waits while the session is cold, and a person's action lets it
/// through.
#[tokio::test]
async fn an_inbox_wake_while_cold_waits_for_a_person() {
    let (shared, _rx) = cold_session().await;
    shared.notify("ci".into(), "build failed".into(), None);
    let wake = tokio::spawn({
        let shared = Arc::clone(&shared);
        async move { shared.inbox_wake_while_warm().await }
    });
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(!wake.is_finished(), "held while cold");
}

/// A resume on another main model starts cold whatever the usage record says: the
/// record's fresh requests cached the old model's prefix, so they seed no touch clock
/// (nothing for the keepalive to ping) and the session is cold from the moment of the
/// switch, until a request on the new prefix is made.
#[test]
fn a_main_switch_starts_the_session_cold() {
    let usage_path = std::env::temp_dir().join("efference-cold-switch-test-usage.jsonl");
    let minute_ago = crate::log::Timestamp::now().0 - 60_000;
    std::fs::write(
        &usage_path,
        format!("{{\"at_ms\":{minute_ago},\"origin\":\"turn\"}}\n"),
    )
    .unwrap();

    let (kept, _rx) = Session::new(std::env::current_dir().unwrap(), Log::in_memory());
    kept.set_cache_ttl(Duration::from_secs(300));
    append_compactable_history(&kept);
    kept.attach_usage_log(&usage_path);
    assert_eq!(kept.cold_since(), None, "a minute-old turn is still warm");

    let (switched, _rx) = Session::new(std::env::current_dir().unwrap(), Log::in_memory());
    switched.set_cache_ttl(Duration::from_secs(300));
    append_compactable_history(&switched);
    let lost = crate::log::Timestamp::now();
    switched.mark_cache_lost(lost);
    switched.attach_usage_log(&usage_path);
    assert_eq!(switched.cold_since(), Some(lost));
    assert_eq!(
        switched.keepalive_clocks(CacheLineage::Main).0,
        None,
        "the old model's request is not a touch of the new prefix"
    );

    std::thread::sleep(Duration::from_millis(2));
    switched.record_request(RequestOrigin::Turn, no_usage());
    assert_eq!(
        switched.cold_since(),
        None,
        "a request on the new prefix ends the belief"
    );
}
