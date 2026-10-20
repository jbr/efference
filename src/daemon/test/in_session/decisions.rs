//! `decide` in-session: each outcome, the moved gate, and the recorded decider
//! (docs/tools.md, docs/permissions.md).

use super::*;
use crate::control_tools::{Decide, Decision};
use crate::frontend::Approval;
use crate::log::Telemetry;

fn decide(seq: &str, decision: Decision, message: Option<&str>) -> Decide {
    Decide {
        agent: Some("child".into()),
        seq: seq.into(),
        decision,
        message: message.map(str::to_string),
    }
}

/// The child's gate decisions, in order.
fn decisions(session: &Session) -> Vec<crate::log::GateDecision> {
    session
        .snapshot()
        .into_iter()
        .filter_map(|e| match e.payload {
            Payload::Telemetry(Telemetry::GateDecision(decision)) => Some(decision),
            _ => None,
        })
        .collect()
}

/// Hold `curl` at the child's gate; returns its handle and the gate's receiver.
fn held(child: &Session) -> (String, tokio::sync::oneshot::Receiver<Approval>) {
    let seq = issue(child, vec![curl()]).to_string();
    (seq, child.register_pending(&curl(), None))
}

/// Allow runs the held call, the answer names it, and the decision is recorded with
/// the parent as its decider.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn allow_runs_the_call_and_records_the_decider() {
    let fixture = Fixture::new();
    let child = fixture.session("child");
    let (seq, gate) = held(&child);
    let said = decide(&seq, Decision::Allow, None)
        .execute(fixture.control("parent"), RequestContext::default())
        .await
        .unwrap();
    assert_eq!(
        said,
        "allowed: child runs bash `curl -sI https://example.com`"
    );
    assert_eq!(gate.await, Ok(Approval::Approve));
    assert_eq!(
        decisions(&child),
        [crate::log::GateDecision::new(
            "toolu_curl",
            Approval::Approve,
            Some("session[parent]".into())
        )]
    );

    fixture.host.swansong().shut_down().await;
}

/// Allow with a note approves, then sends the note as the parent's turn, which the
/// child reads with the call's result.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn allow_with_a_note_sends_the_note_after_the_result() {
    let fixture = Fixture::new();
    let child = fixture.session("child");
    let (seq, gate) = held(&child);
    let said = decide(&seq, Decision::Allow, Some("use read for this next time"))
        .execute(fixture.control("parent"), RequestContext::default())
        .await
        .unwrap();
    assert_eq!(
        said,
        "allowed with your note: child runs bash `curl -sI https://example.com`; your note \
         arrives with its result"
    );
    assert_eq!(gate.await, Ok(Approval::Approve));
    let noted = || {
        child.queued_turns().iter().any(|turn| {
            turn.text == "use read for this next time"
                && turn.source.as_deref() == Some("session[parent]")
        }) || child
            .snapshot()
            .iter()
            .any(|e| e.source.as_deref() == Some("session[parent]"))
    };
    wait_for("the note reached the child", noted).await;
    // The decision names the note's turn, so a reader of the log can tell it from any
    // other message the parent sends.
    let recorded = decisions(&child);
    let named = recorded[0]
        .note_submission
        .clone()
        .expect("the note is named");
    let sent = child
        .queued_turns()
        .iter()
        .find(|turn| turn.text == "use read for this next time")
        .map(|turn| turn.submission_id.clone())
        .or_else(|| {
            child
                .snapshot()
                .iter()
                .find(|e| e.source.as_deref() == Some("session[parent]"))
                .and_then(|e| e.submission_id.clone())
        });
    assert_eq!(sent, Some(named));

    fixture.host.swansong().shut_down().await;
}

/// Deny with a message refuses the call and steers the turn; a bare deny ends it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn deny_redirects_with_a_message_and_aborts_without_one() {
    let fixture = Fixture::new();
    let child = fixture.session("child");
    let (seq, gate) = held(&child);
    let said = decide(&seq, Decision::Deny, Some("use the cached copy"))
        .execute(fixture.control("parent"), RequestContext::default())
        .await
        .unwrap();
    assert_eq!(said, "denied: child continues under your message");
    assert_eq!(
        gate.await,
        Ok(Approval::Redirect("use the cached copy".into()))
    );

    let (seq, gate) = held(&child);
    let said = decide(&seq, Decision::Deny, None)
        .execute(fixture.control("parent"), RequestContext::default())
        .await
        .unwrap();
    assert_eq!(said, "denied: child's turn stopped");
    assert_eq!(gate.await, Ok(Approval::Abort));

    fixture.host.swansong().shut_down().await;
}

/// A seq that is not the held call is refused with what the session holds now, and a
/// session holding nothing says what it is doing instead; neither resolves anything.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_moved_gate_is_refused_with_what_it_holds_now() {
    let fixture = Fixture::new();
    let child = fixture.session("child");
    let err = decide("4", Decision::Allow, None)
        .execute(fixture.control("parent"), RequestContext::default())
        .await
        .unwrap_err()
        .to_string();
    assert!(
        err.starts_with("gate moved: 4 is not waiting for a decision; now child: idle"),
        "{err}"
    );

    let (seq, _gate) = held(&child);
    let stale = format!("{}", seq.parse::<u64>().unwrap() + 3);
    let err = decide(&stale, Decision::Allow, None)
        .execute(fixture.control("parent"), RequestContext::default())
        .await
        .unwrap_err()
        .to_string();
    assert_eq!(
        err,
        format!(
            "gate moved: {stale} is not waiting for a decision; now child: waiting for your \
             decision on bash `curl -sI https://example.com` (seq {seq})"
        )
    );
    assert!(decisions(&child).is_empty(), "nothing was decided");
    assert!(child.pending_gate().is_some());

    fixture.host.swansong().shut_down().await;
}

/// Only the supervisor decides: a peer is refused with who decides and what reaches the
/// child from it, and the gate stays held.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_peer_cannot_decide() {
    let fixture = Fixture::new();
    let child = fixture.session("child");
    let (seq, _gate) = held(&child);
    let err = decide(&seq, Decision::Allow, None)
        .execute(fixture.control("peer"), RequestContext::default())
        .await
        .unwrap_err()
        .to_string();
    assert_eq!(
        err,
        "refused: you don't supervise \"child\", so its gates are not yours to decide — its \
         supervisor, \"parent\", and the operator decide them; agents__message leaves it a note"
    );
    assert!(child.pending_gate().is_some());

    fixture.host.swansong().shut_down().await;
}

/// Driving is not deciding (docs/permissions.md): `parent` drives `grandchild` through
/// the creation chain, and a successor drives its predecessor, but neither supervises
/// it, so both are refused `decide` — an allow and a deny alike — naming who does. The
/// drive relation is otherwise unchanged: the same caller still interrupts it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_driver_that_does_not_supervise_cannot_decide() {
    let fixture = Fixture::new();
    let grandchild = fixture.session("grandchild");
    let seq = issue(&grandchild, vec![curl()]).to_string();
    let _gate = grandchild.register_pending(&curl(), None);
    let refused = |caller: &str, agent: &str, decision: Decision| {
        let control = fixture.control(caller);
        let seq = seq.clone();
        let agent = agent.to_string();
        async move {
            Decide {
                agent: Some(agent),
                ..decide(&seq, decision, None)
            }
            .execute(control, RequestContext::default())
            .await
            .unwrap_err()
            .to_string()
        }
    };
    let expected = "refused: you don't supervise \"grandchild\", so its gates are not yours to \
        decide — its supervisor, \"child\", and the operator decide them; agents__message and \
        agents__interrupt still reach it";
    assert_eq!(
        refused("parent", "grandchild", Decision::Allow).await,
        expected
    );
    assert_eq!(
        refused("parent", "grandchild", Decision::Deny).await,
        expected
    );
    assert!(grandchild.pending_gate().is_some());
    assert!(decisions(&grandchild).is_empty(), "nothing was decided");
    Interrupt {
        agent: Some("grandchild".into()),
    }
    .execute(fixture.control("parent"), RequestContext::default())
    .await
    .expect("parent still drives its grandchild");

    // `peer` succeeds `parent`, a root, and drives it; a decision is still refused.
    fixture
        .host
        .record_succession("parent", "peer", "carry on".into());
    let err = refused("peer", "parent", Decision::Deny).await;
    assert_eq!(
        err,
        "refused: you don't supervise \"parent\", so its gates are not yours to decide — it \
         has no supervisor, so the operator decides them; agents__message and agents__interrupt \
         still reach it"
    );

    fixture.host.swansong().shut_down().await;
}
