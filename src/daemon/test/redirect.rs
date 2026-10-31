//! A redirect names who decided it (docs/permissions.md): the stub the model reads, the
//! user turn carrying the message, and the gate decision on the log.

use super::upstream_hold::{Answer, end_turn, scripted, start_with, tool_call, wait_until};
use super::*;
use crate::frontend::SubmittedTurn;
use crate::log::{Approval, ContentBlock, Stance, Telemetry};
use std::sync::atomic::AtomicUsize;

fn script(n: usize) -> Answer {
    match n {
        0 => Answer::Stream(tool_call("toolu_1", "bash", r#"{"command":"rm -rf tmp"}"#)),
        _ => Answer::Stream(end_turn()),
    }
}

/// The stub result for `toolu_1` and the redirect's user turn, once both are logged.
fn stub_and_steer(session: &Session) -> Option<(String, Option<String>, String)> {
    let log = session.snapshot();
    let stub = log.iter().find_map(|e| match &e.payload {
        Payload::ToolResult { call_id, raw, .. } if call_id.0 == "toolu_1" => {
            Some(raw.content.clone())
        }
        _ => None,
    })?;
    let steer = log.iter().find_map(|e| match &e.payload {
        Payload::User(blocks) if e.submission_id.is_none() => match blocks.as_slice() {
            [ContentBlock::Text(text)] => Some((e.source.clone(), text.clone())),
            _ => None,
        },
        _ => None,
    })?;
    Some((stub, steer.0, steer.1))
}

/// A supervisor's deny with a message: the stub names it as its messages open, the turn
/// carrying its message carries its source, and the wire opens that turn `<id> (agent):`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_supervisors_deny_names_the_supervisor() {
    let requests = Arc::new(AtomicUsize::new(0));
    let addr = scripted(Arc::clone(&requests), script);
    let (_host, runtime) = start_with(addr, |boot| {
        boot.spotter.stance = Stance::Gate;
        boot.spotter.reason = "level 3: destructive".into();
    });
    let session = Arc::clone(runtime.session());
    session.submit_turn(SubmittedTurn::new("go", "s1"));
    wait_until("the call is held", || session.pending_gate().is_some()).await;
    assert!(session.resolve_approval(
        &CallId("toolu_1".into()),
        Approval::Redirect("use trash instead".into()),
        Some("session[parent]".into()),
        None,
    ));
    wait_until("the redirect has landed", || {
        stub_and_steer(&session).is_some()
    })
    .await;

    let (stub, source, text) = stub_and_steer(&session).unwrap();
    assert_eq!(
        stub,
        "Not run — parent (agent) replied instead of approving; see the next message."
    );
    assert_eq!(source.as_deref(), Some("session[parent]"));
    assert_eq!(text, "use trash instead");
    let wire = serde_json::to_string(&session.project_messages()).unwrap();
    assert!(wire.contains("parent (agent):"), "{wire}");
}

/// The autonomous redirect is the permission review's: it is recorded as a gate decision
/// under `review[autonomous]`, its stub says so, and its turn carries no source.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_autonomous_redirect_is_the_reviews() {
    let requests = Arc::new(AtomicUsize::new(0));
    let addr = scripted(Arc::clone(&requests), script);
    let (_host, runtime) = start_with(addr, |boot| {
        boot.autonomous = true;
        boot.spotter.stance = Stance::Watch;
        boot.spotter.reason = "watch-for: rm".into();
    });
    let session = Arc::clone(runtime.session());
    session.submit_turn(SubmittedTurn::new("go", "s1"));
    wait_until("the redirect has landed", || {
        stub_and_steer(&session).is_some()
    })
    .await;

    let (stub, source, _) = stub_and_steer(&session).unwrap();
    assert_eq!(
        stub,
        "Not run — the permission review declined it; see the next message."
    );
    assert_eq!(source, None, "the review's own text names itself");
    let decider = session.snapshot().iter().find_map(|e| match &e.payload {
        Payload::Telemetry(Telemetry::GateDecision(decision)) => decision.source.clone(),
        _ => None,
    });
    assert_eq!(decider.as_deref(), Some("review[autonomous]"));
}
