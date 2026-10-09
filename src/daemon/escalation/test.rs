//! Tests for `escalation.rs`: whether a moment sends now or waits, and what ends
//! the wait. The send itself is not observable (no root is installed in tests), so
//! what is asserted is the pending slot: a message in it is a push that has not
//! gone out, and an empty slot after the delay is one that fired.

use super::*;
use crate::daemon::attention::{Attention, AttentionReport};
use crate::frontend::Approval;
use crate::log::{CallId, ToolCall};
use crate::persist::Log;

fn session() -> Arc<Session> {
    Session::new(std::env::current_dir().unwrap(), Log::in_memory()).0
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
            viewing: None,
            active,
        },
        std::time::Instant::now(),
    );
}

fn titles(shared: &Session) -> Option<String> {
    shared.pending_escalation().map(|m| m.title)
}

/// A session outside any host, and a host with nobody attentive, send at once:
/// nothing is held.
#[tokio::test(start_paused = true)]
async fn an_unattended_moment_is_not_held() {
    let shared = session();
    shared.push("Gate", "body".into());
    assert_eq!(titles(&shared), None);

    let (shared, attention) = attended_session();
    set_active(&attention, false);
    shared.push("Gate", "body".into());
    assert_eq!(titles(&shared), None);
}

/// An attended moment is held, carrying the session's web path, and goes out
/// after [`ESCALATION_DELAY`] if nothing cancelled it.
#[tokio::test(start_paused = true)]
async fn an_attended_moment_is_held_then_fires() {
    let (shared, _attention) = attended_session();
    shared.set_web_path("/sessions/hardy-hawk/".into());
    shared.push("Gate", "body".into());
    let held = shared.pending_escalation().expect("held");
    assert!(held.title.starts_with("Gate"));
    assert_eq!(held.path, "/sessions/hardy-hawk/");

    tokio::time::sleep(ESCALATION_DELAY - Duration::from_secs(1)).await;
    assert!(
        shared.pending_escalation().is_some(),
        "not before the delay"
    );

    tokio::time::sleep(Duration::from_secs(2)).await;
    assert_eq!(titles(&shared), None, "fired and cleared");
}

/// One escalation per session: a newer moment replaces the older, and the older
/// timer does not fire the newer one early.
#[tokio::test(start_paused = true)]
async fn a_newer_moment_supersedes_the_older() {
    let (shared, _attention) = attended_session();
    shared.push("Gate", "first".into());
    tokio::time::sleep(Duration::from_secs(60)).await;
    shared.push("Message", "second".into());
    assert!(titles(&shared).unwrap().starts_with("Message"));

    // The first timer would have fired here.
    tokio::time::sleep(Duration::from_secs(61)).await;
    assert!(shared.pending_escalation().is_some());
    tokio::time::sleep(Duration::from_secs(60)).await;
    assert_eq!(titles(&shared), None);
}

/// A newer moment with nobody attending sends now and drops the older held one.
#[tokio::test(start_paused = true)]
async fn an_unattended_moment_supersedes_a_held_one() {
    let (shared, attention) = attended_session();
    shared.push("Gate", "first".into());
    set_active(&attention, false);
    shared.push("Message", "second".into());
    assert_eq!(titles(&shared), None);
}

/// A decided gate cancels its escalation, whichever way it was decided.
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
        let _gate = shared.register_pending(&call, None);
        assert!(shared.pending_escalation().is_some());
        shared.resolve_approval(&call.id, decision);
        assert_eq!(titles(&shared), None);
    }
}

/// Answering an `ask` — a turn submitted while it is pending — cancels.
#[tokio::test(start_paused = true)]
async fn answering_an_ask_cancels() {
    let (shared, _attention) = attended_session();
    let _wait = shared.begin_ask("ask_1", "Sure?".into(), 1, 1);
    let held = shared.pending_escalation().expect("the ask is held");
    assert_eq!(held.body, "Sure?");
    shared.submit_turn(crate::frontend::SubmittedTurn::new("yes", "sub_1"));
    assert_eq!(titles(&shared), None);
}

/// An `ask` that ends without a reply (an interrupt) cancels too.
#[tokio::test(start_paused = true)]
async fn an_ask_that_ends_cancels() {
    let (shared, _attention) = attended_session();
    let wait = shared.begin_ask("ask_1", "Sure?".into(), 1, 1);
    assert!(shared.pending_escalation().is_some());
    drop(wait);
    assert_eq!(titles(&shared), None);
}

/// A turn submitted after a settle resolves it.
#[tokio::test(start_paused = true)]
async fn submitting_a_turn_cancels_a_settle() {
    let (shared, _attention) = attended_session();
    shared.push("Message", "done".into());
    assert!(shared.pending_escalation().is_some());
    shared.submit_turn(crate::frontend::SubmittedTurn::new("next", "sub_1"));
    assert_eq!(titles(&shared), None);
}
