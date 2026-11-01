//! A session's status as a subscriber hears it (docs/tools.md).

use super::*;
use crate::protocol::state::{BlockedReason, LoopPhase};
use crate::record::log::Approval;
use crate::session::status::SessionStatus;

fn session() -> Arc<Session> {
    Session::in_memory().0
}

/// The status reads the loop phase: a request is running, a held call is gated, a
/// triage held without a verdict is the stall it publishes, and an idle loop whose
/// trajectory gave up is stalled.
#[test]
fn the_status_follows_the_loop_phase() {
    let shared = session();
    assert_eq!(shared.status(), SessionStatus::Idle);

    shared.set_phase(LoopPhase::Requesting);
    assert_eq!(shared.status(), SessionStatus::Running);

    let call = ToolCall::new(
        "bash",
        "toolu_1",
        serde_json::json!({ "command": "ls ~", "intent": "look around" }),
    );
    shared.set_phase(LoopPhase::Executing);
    let _rx = shared.register_pending(&call, Default::default());
    assert_eq!(
        shared.status(),
        SessionStatus::Gated {
            call_id: "toolu_1".into(),
            name: "bash".into(),
            intent: Some("look around".into()),
            reason: None,
        }
    );
    assert!(shared.resolve_approval(
        &CallId("toolu_1".into()),
        Approval::Abort,
        Default::default()
    ));
    assert_eq!(
        shared.status(),
        SessionStatus::Running,
        "the decision ends the gate before the loop wakes"
    );

    let stall = |reason| {
        agent::stalled(
            reason,
            None,
            "the trajectory stopped: send a turn to retry".into(),
            "boom".into(),
        )
    };
    let stalled = SessionStatus::Stalled {
        message: "the trajectory stopped: send a turn to retry".into(),
    };
    shared.set_stall(stall(StallReason::TriageUnavailable));
    {
        let _held = shared.wait_phase(
            LoopPhase::Blocked {
                reason: BlockedReason::TriageUnavailable,
            },
            LoopPhase::Requesting,
        );
        assert_eq!(shared.status(), stalled);
    }
    shared.clear_stall();
    assert_eq!(shared.status(), SessionStatus::Running);

    shared.set_stall(stall(StallReason::RequestFailed));
    shared.going_idle();
    assert_eq!(shared.status(), stalled);
    shared.clear_stall();
    assert_eq!(shared.status(), SessionStatus::Idle);

    shared.set_phase(LoopPhase::Requesting);
    shared.set_pace(None, Some(true));
    {
        let _held = shared.wait_phase(
            LoopPhase::Blocked {
                reason: BlockedReason::PaceHeld,
            },
            LoopPhase::Requesting,
        );
        assert_eq!(
            shared.status(),
            SessionStatus::Blocked {
                reason: BlockedReason::PaceHeld
            }
        );
        shared.set_pace(None, Some(false));
        assert_eq!(
            shared.status(),
            SessionStatus::Running,
            "the release ends the hold before the loop wakes"
        );
    }
}

/// An idle triage reads as running — to the status watcher and to `/settle` — until it
/// starts a trajectory that ends, or returns to idle with nothing pinned; only
/// the trajectory counts as a turn ended.
#[test]
fn an_idle_triage_reads_as_running_until_it_settles() {
    let shared = session();
    assert!(shared.is_settled());

    shared.idle_triage_started();
    assert_eq!(shared.status(), SessionStatus::Running);
    assert!(
        !shared.is_settled(),
        "an idle triage is a request in flight"
    );
    shared.idle_triage_settled();
    shared.going_idle();
    assert_eq!(shared.status(), SessionStatus::Idle);
    assert!(shared.is_settled());
    assert_eq!(shared.turns_ended(), 0, "no turn ran");

    shared.idle_triage_started();
    shared.going_idle();
    assert_eq!(
        shared.turns_ended(),
        1,
        "a triage that started a trajectory ran one"
    );
}
