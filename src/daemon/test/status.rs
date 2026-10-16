//! A session's status as a subscriber hears it (docs/tools.md).

use super::*;
use crate::daemon::status::{SessionStatus, status_slot};
use crate::daemon::{BlockedReason, LoopPhase};
use crate::frontend::Approval;

fn session() -> Arc<Session> {
    Session::new(std::env::current_dir().unwrap(), Log::in_memory()).0
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
    let _rx = shared.register_pending(&call, None);
    assert_eq!(
        shared.status(),
        SessionStatus::Gated {
            call_id: "toolu_1".into(),
            name: "bash".into(),
            intent: Some("look around".into()),
            reason: None,
        }
    );
    assert!(shared.resolve_approval(&CallId("toolu_1".into()), Approval::Abort));
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
/// escalates into a trajectory that ends, or returns to idle with nothing pinned; only
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
        "an escalated triage ran a trajectory"
    );
}

/// The gate body names the tool, the intent, the spotter's reason, the held
/// `call_id`, and the verb that resolves it; the idle body carries the turn's
/// closing prose; every body says where the log stands.
#[test]
fn status_bodies_name_the_condition_and_the_next_call() {
    let shared = session();
    let gated = SessionStatus::Gated {
        call_id: "toolu_1".into(),
        name: "fs__list".into(),
        intent: Some("locating the sibling servers".into()),
        reason: Some("outside the granted paths".into()),
    };
    let body = shared.status_body("child", &gated);
    assert_eq!(
        body,
        "child is gated on fs.list — locating the sibling servers (with an empty log). Held \
         because: outside the granted paths. Nothing runs until call_id toolu_1 is resolved \
         with sessions__approve (approve, abort, or redirect); sessions__state shows the \
         held call."
    );

    shared
        .append(
            1,
            Payload::Assistant {
                output: crate::log::AssistantOutput::from_legacy_messages(
                    vec![],
                    vec![ContentBlock::Text("done: two files changed".into())],
                    vec![],
                ),
                upstream_failure: None,
            },
        )
        .unwrap();
    assert_eq!(
        shared.status_body("child", &SessionStatus::Idle),
        "child finished its turn and is idle at seq 0; sessions__transcript shows what it \
         did. It said: done: two files changed"
    );
    assert_eq!(
        shared.status_body("child", &SessionStatus::Ended),
        "child ended (at seq 0); its log stays on disk."
    );
    assert_eq!(status_slot("child"), "session:child");
}
