//! Tests for `delayed_push.rs`: whether a moment sends now or waits, and what ends
//! the wait. The send itself is not observable (no root is installed in tests), so
//! what is asserted is the pending slot: a message in it is a push that has not
//! gone out, and an empty slot after the delay is one that fired.

use super::*;
use crate::daemon::attention::Attention;
use crate::log::Telemetry;
use crate::log::{Approval, MomentKind};
use crate::log::{CallId, ToolCall};
use crate::protocol::attention::AttentionReport;

fn session() -> Arc<Session> {
    Session::in_memory().0
}

/// A session whose host has one client reporting `active`.
fn attended_session() -> (Arc<Session>, Arc<Attention>) {
    let shared = session();
    let attention = Arc::new(Attention::default());
    shared.set_attention(Arc::clone(&attention));
    set_active(&attention, true);
    (shared, attention)
}

fn set_active(attention: &Attention, active: bool) {
    attention.report_at(
        &AttentionReport {
            client: "c1".into(),
            session: None,
            active,
        },
        Instant::now(),
    );
}

fn titles(shared: &Session) -> Option<String> {
    shared.pending_delayed_push().map(|m| m.title)
}

/// A session outside any host, and a host with nobody attentive, send at once:
/// nothing is held.
#[tokio::test(start_paused = true)]
async fn an_unattended_moment_is_not_held() {
    let shared = session();
    shared.push(MomentKind::Gate, "body".into());
    assert_eq!(titles(&shared), None);

    let (shared, attention) = attended_session();
    set_active(&attention, false);
    shared.push(MomentKind::Gate, "body".into());
    assert_eq!(titles(&shared), None);
}

/// An attended moment is held, carrying the session's web path, and goes out
/// after [`PUSH_DELAY`] if nothing cancelled it.
#[tokio::test(start_paused = true)]
async fn an_attended_moment_is_held_then_fires() {
    let (shared, _attention) = attended_session();
    shared.set_web_path("/sessions/hardy-hawk/".into());
    shared.push(MomentKind::Gate, "body".into());
    let held = shared.pending_delayed_push().expect("held");
    assert!(held.title.starts_with("Gate"));
    assert_eq!(held.path, "/sessions/hardy-hawk/");

    tokio::time::sleep(PUSH_DELAY - Duration::from_secs(1)).await;
    assert!(
        shared.pending_delayed_push().is_some(),
        "not before the delay"
    );

    tokio::time::sleep(Duration::from_secs(2)).await;
    assert_eq!(titles(&shared), None, "fired and cleared");
}

/// One delayed push per session: a newer moment replaces the older, and the older
/// timer does not fire the newer one early.
#[tokio::test(start_paused = true)]
async fn a_newer_moment_supersedes_the_older() {
    let (shared, _attention) = attended_session();
    shared.push(MomentKind::Gate, "first".into());
    tokio::time::sleep(Duration::from_secs(60)).await;
    shared.push(MomentKind::Settle, "second".into());
    assert!(titles(&shared).unwrap().starts_with("Message"));

    // The first timer would have fired here.
    tokio::time::sleep(Duration::from_secs(61)).await;
    assert!(shared.pending_delayed_push().is_some());
    tokio::time::sleep(Duration::from_secs(60)).await;
    assert_eq!(titles(&shared), None);
}

/// A newer moment with nobody attending sends now and drops the older held one.
#[tokio::test(start_paused = true)]
async fn an_unattended_moment_supersedes_a_held_one() {
    let (shared, attention) = attended_session();
    shared.push(MomentKind::Gate, "first".into());
    set_active(&attention, false);
    shared.push(MomentKind::Settle, "second".into());
    assert_eq!(titles(&shared), None);
}

/// A decided gate cancels its delayed push, whichever way it was decided.
#[tokio::test(start_paused = true)]
async fn deciding_a_gate_cancels() {
    for decision in [
        Approval::Approve,
        Approval::Abort,
        Approval::Redirect("no".into()),
    ] {
        let (shared, _attention) = attended_session();
        let call = ToolCall::new(
            "bash",
            CallId("call_1".into()),
            serde_json::json!({"command": "ls"}),
        );
        let _gate = shared.register_pending(&call, None, None);
        assert!(shared.pending_delayed_push().is_some());
        shared.resolve_approval(&call.id, decision, None, None);
        assert_eq!(titles(&shared), None);
    }
}

/// Answering an `ask` — a turn submitted while it is pending — cancels.
#[tokio::test(start_paused = true)]
async fn answering_an_ask_cancels() {
    let (shared, _attention) = attended_session();
    let _wait = shared.begin_ask("ask_1", "Sure?".into(), 1, 1);
    let held = shared.pending_delayed_push().expect("the ask is held");
    assert_eq!(held.body, "Sure?");
    shared.submit_client_turn(SubmittedTurn::new("yes", "sub_1"));
    assert_eq!(titles(&shared), None);
}

/// An `ask` that ends without a reply (an interrupt) cancels too.
#[tokio::test(start_paused = true)]
async fn an_ask_that_ends_cancels() {
    let (shared, _attention) = attended_session();
    let wait = shared.begin_ask("ask_1", "Sure?".into(), 1, 1);
    assert!(shared.pending_delayed_push().is_some());
    drop(wait);
    assert_eq!(titles(&shared), None);
}

/// A turn submitted after a settle resolves it.
#[tokio::test(start_paused = true)]
async fn submitting_a_turn_cancels_a_settle() {
    let (shared, _attention) = attended_session();
    shared.push(MomentKind::Settle, "done".into());
    assert!(shared.pending_delayed_push().is_some());
    shared.submit_client_turn(SubmittedTurn::new("next", "sub_1"));
    assert_eq!(titles(&shared), None);
}

// The push record (docs/web-push.md): every step of a moment lands in the log.

use crate::log::{
    AttentiveClient, DeliveryOutcome, DeviceDelivery, Payload, PushCancel, PushEvent, PushRecord,
};

fn records(shared: &Session) -> Vec<PushRecord> {
    shared
        .log
        .lock()
        .unwrap()
        .entries()
        .iter()
        .filter_map(|e| match &e.payload {
            Payload::Telemetry(Telemetry::Push(record)) => Some(record.clone()),
            _ => None,
        })
        .collect()
}

fn events(shared: &Session) -> Vec<PushEvent> {
    records(shared).into_iter().map(|r| r.event).collect()
}

fn device(outcome: DeliveryOutcome) -> DeviceDelivery {
    DeviceDelivery {
        host: "push.example.net".into(),
        device: "0badc0de".into(),
        outcome,
    }
}

/// A transport that answers every push with `devices`.
fn stub(shared: &Session, devices: Vec<DeviceDelivery>) {
    shared.set_push_transport(Arc::new(move |_message| {
        let devices = devices.clone();
        Box::pin(async move { devices })
    }));
}

async fn settle_tasks() {
    tokio::time::sleep(Duration::from_millis(1)).await;
}

/// Nobody attending: the decision is `sent`, and the push services' answer
/// follows under the same moment.
#[tokio::test(start_paused = true)]
async fn an_unattended_moment_records_sent_then_delivery() {
    let shared = session();
    let answer = device(DeliveryOutcome::Delivered { status: 201 });
    stub(&shared, vec![answer.clone()]);
    shared.push(MomentKind::Gate, "body".into());
    settle_tasks().await;
    let records = records(&shared);
    assert_eq!(records.len(), 2);
    assert!(
        records
            .iter()
            .all(|r| r.moment == 1 && r.kind == MomentKind::Gate)
    );
    assert_eq!(records[0].event, PushEvent::Sent);
    assert_eq!(
        records[1].event,
        PushEvent::Delivery {
            devices: vec![answer]
        }
    );
}

/// A send with no subscriptions is recorded as an empty delivery.
#[tokio::test(start_paused = true)]
async fn a_send_to_no_devices_is_recorded() {
    let shared = session();
    stub(&shared, vec![]);
    shared.push(MomentKind::Settle, "done".into());
    settle_tasks().await;
    assert_eq!(
        events(&shared),
        vec![PushEvent::Sent, PushEvent::Delivery { devices: vec![] }]
    );
}

/// A gone device and a failure are recorded with their detail.
#[tokio::test(start_paused = true)]
async fn failed_and_gone_devices_are_recorded() {
    let shared = session();
    let devices = vec![
        device(DeliveryOutcome::Gone { status: 410 }),
        device(DeliveryOutcome::Failed {
            status: Some(503),
            reason: "status 503".into(),
        }),
    ];
    stub(&shared, devices.clone());
    shared.push(MomentKind::Ask, "Sure?".into());
    settle_tasks().await;
    assert_eq!(events(&shared)[1], PushEvent::Delivery { devices });
}

/// An attended moment records `held` with who was attending, then `released`
/// and the delivery once the delay passes.
#[tokio::test(start_paused = true)]
async fn a_held_moment_records_held_released_delivery() {
    let (shared, _attention) = attended_session();
    stub(
        &shared,
        vec![device(DeliveryOutcome::Delivered { status: 201 })],
    );
    shared.push(MomentKind::Gate, "body".into());
    assert_eq!(
        events(&shared),
        vec![PushEvent::Held {
            delay_secs: PUSH_DELAY.as_secs(),
            attentive: vec![AttentiveClient {
                client: "c1".into(),
                session: None
            }],
        }]
    );
    tokio::time::sleep(PUSH_DELAY + Duration::from_secs(1)).await;
    let events = events(&shared);
    assert_eq!(events.len(), 3);
    assert_eq!(events[1], PushEvent::Released);
    assert!(matches!(events[2], PushEvent::Delivery { .. }));
}

fn cancelled(shared: &Session) -> Vec<PushCancel> {
    events(shared)
        .into_iter()
        .filter_map(|e| match e {
            PushEvent::Cancelled { cause } => Some(cause),
            _ => None,
        })
        .collect()
}

/// Each way a held push is dropped names its cause.
#[tokio::test(start_paused = true)]
async fn each_cancel_cause_is_recorded() {
    // A decided gate.
    let (shared, _attention) = attended_session();
    let call = ToolCall::new(
        "bash",
        CallId("call_1".into()),
        serde_json::json!({"command": "ls"}),
    );
    let _gate = shared.register_pending(&call, None, None);
    shared.resolve_approval(&call.id, Approval::Approve, None, None);
    assert_eq!(cancelled(&shared), vec![PushCancel::GateDecided]);

    // An ask that ends.
    let (shared, _attention) = attended_session();
    drop(shared.begin_ask("ask_1", "Sure?".into(), 1, 1));
    assert_eq!(cancelled(&shared), vec![PushCancel::AskEnded]);

    // A turn submitted.
    let (shared, _attention) = attended_session();
    shared.push(MomentKind::Settle, "done".into());
    shared.submit_client_turn(SubmittedTurn::new("next", "sub_1"));
    assert_eq!(cancelled(&shared), vec![PushCancel::TurnSubmitted]);

    // Archived and seen.
    for cause in [
        PushCancel::Archived,
        PushCancel::Seen {
            client: "c1".into(),
        },
    ] {
        let (shared, _attention) = attended_session();
        shared.push(MomentKind::Gate, "body".into());
        shared.cancel_delayed_push(cause.clone());
        assert_eq!(cancelled(&shared), vec![cause]);
    }

    // A newer moment, attended or not.
    let (shared, attention) = attended_session();
    shared.push(MomentKind::Gate, "first".into());
    shared.push(MomentKind::Settle, "second".into());
    assert_eq!(cancelled(&shared), vec![PushCancel::Superseded]);
    set_active(&attention, false);
    shared.push(MomentKind::Gate, "third".into());
    assert_eq!(
        cancelled(&shared),
        vec![PushCancel::Superseded, PushCancel::Superseded]
    );
    // The cancelled entry names the older moment.
    let superseded = records(&shared)
        .into_iter()
        .find(|r| matches!(r.event, PushEvent::Cancelled { .. }))
        .unwrap();
    assert_eq!((superseded.moment, superseded.kind), (1, MomentKind::Gate));
}

/// Cancelling with nothing pending records nothing.
#[tokio::test(start_paused = true)]
async fn a_cancel_with_nothing_pending_records_nothing() {
    let shared = session();
    shared.cancel_delayed_push(PushCancel::Archived);
    assert!(records(&shared).is_empty());
}
