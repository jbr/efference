//! A session's status as a subscriber hears it (docs/tools.md).

use super::*;
use crate::daemon::status::{SessionStatus, status_slot};
use crate::frontend::Approval;

fn session() -> Arc<Session> {
    Session::new(std::env::current_dir().unwrap(), Log::in_memory()).0
}

/// Precedence: a held call outranks a stall, a stall outranks the running flag,
/// and neither set means the loop is idle.
#[test]
fn a_gate_outranks_a_stall_which_outranks_running() {
    let shared = session();
    assert_eq!(shared.status(), SessionStatus::Idle);

    shared.running.store(true, Ordering::SeqCst);
    assert_eq!(shared.status(), SessionStatus::Running);

    shared.set_stall(agent::stalled(
        StallReason::RequestFailed,
        None,
        "the trajectory stopped: send a turn to retry".into(),
        "boom".into(),
    ));
    assert_eq!(
        shared.status(),
        SessionStatus::Stalled {
            message: "the trajectory stopped: send a turn to retry".into()
        }
    );

    let call = ToolCall::new(
        "bash",
        "toolu_1",
        serde_json::json!({ "command": "ls ~", "intent": "look around" }),
    );
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
    shared.clear_stall();
    shared.running.store(false, Ordering::SeqCst);
    assert_eq!(shared.status(), SessionStatus::Idle);
}

/// A trajectory that starts from an idle-triage escalation (no submitted turn to
/// claim) still reads as running — to the status watcher and to `/settle` — until
/// the loop parks again.
#[test]
fn an_escalated_trajectory_reads_as_running_until_it_parks() {
    let shared = session();
    assert!(shared.is_settled());

    shared.escalated();
    assert_eq!(shared.status(), SessionStatus::Running);
    assert!(
        !shared.is_settled(),
        "mid-trajectory is not a decision point"
    );

    shared.going_idle();
    assert_eq!(shared.status(), SessionStatus::Idle);
    assert!(shared.is_settled());
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
