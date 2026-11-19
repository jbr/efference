//! The supervision relation on the permission path (docs/permissions.md): the facts the
//! spotter is told, and the archive of a session the caller supervises, which clears on
//! the fast path.

use super::*;
use crate::protocol::agents::{ARCHIVE_WIRE_NAME, Supervisee, Supervision};
use crate::protocol::sessions::ListedStatus;
use crate::session::review::SupervisorPointers;

/// A host's supervisor pointers as a table of (supervised, supervisor) pairs, every
/// session in it hosted, idle, and created by its supervisor.
struct Pointers(Vec<(&'static str, &'static str)>);

impl SupervisorPointers for Pointers {
    fn supervisor_of(&self, id: &str) -> Option<String> {
        self.0
            .iter()
            .find(|(supervised, _)| *supervised == id)
            .map(|(_, supervisor)| supervisor.to_string())
    }

    fn supervisees(&self, id: &str) -> Vec<Supervisee> {
        self.0
            .iter()
            .filter(|(_, supervisor)| *supervisor == id)
            .map(|(supervised, _)| Supervisee {
                id: supervised.to_string(),
                created: true,
                status: ListedStatus::Idle,
            })
            .collect()
    }

    fn holds(&self, _id: &str) -> bool {
        true
    }
}

/// `me`, supervised by `lead` and supervising `kid`; `other` supervises `elsewhere`.
/// The pointers are returned with the session, which holds them weakly.
fn me() -> (Arc<Session>, Arc<Pointers>) {
    let pointers = Arc::new(Pointers(vec![
        ("me", "lead"),
        ("kid", "me"),
        ("elsewhere", "other"),
    ]));
    let shared = parent_with(crate::config::Config::default());
    shared.link_supervisor(&pointers, "me".into());
    (shared, pointers)
}

fn archive(target: &str) -> ToolCall {
    ToolCall::new(
        ARCHIVE_WIRE_NAME,
        "toolu_archive",
        serde_json::json!({ "agent": target }),
    )
}

fn recording() -> RecordingSpotter {
    RecordingSpotter {
        seen: std::sync::Mutex::new(Vec::new()),
        stance: Stance::Gate,
        reason: "would gate".into(),
    }
}

/// Archiving a session the caller supervises clears with no spotter request and no
/// `Spot` entry, as a denial at a child's gate does (docs/permissions.md).
#[tokio::test]
async fn archiving_a_session_it_supervises_clears_without_the_spotter() {
    let (shared, _pointers) = me();
    let approval = judge(
        &mut NeverGates,
        &shared,
        &NeverAsked,
        &Toolbox::bash_only(),
        1,
        &archive("kid"),
        None,
    )
    .await
    .unwrap();
    assert_eq!(approval, Approval::Approve);
    assert!(
        !shared
            .snapshot()
            .iter()
            .any(|e| matches!(&e.payload, Payload::Spot(_))),
        "no spotter verdict was asked for or logged"
    );
}

/// An archive of any other session, its own supervisor included, goes through the
/// ladder: the spotter is asked, and its gate reaches the human.
#[tokio::test]
async fn archiving_a_session_it_does_not_supervise_goes_to_the_spotter() {
    let (shared, _pointers) = me();
    for target in ["elsewhere", "lead", "nobody"] {
        let spotter = recording();
        let mut frontend = RecordingFrontend::answering(Approval::Abort);
        let approval = judge(
            &mut frontend,
            &shared,
            &spotter,
            &Toolbox::bash_only(),
            1,
            &archive(target),
            None,
        )
        .await
        .unwrap();
        assert_eq!(approval, Approval::Abort, "{target}");
        assert_eq!(
            spotter.seen.lock().unwrap()[0].0,
            ARCHIVE_WIRE_NAME,
            "{target}: the spotter judged the archive"
        );
        assert_eq!(
            frontend.decisions.len(),
            1,
            "{target}: the gate was reached"
        );
    }
}

/// The spotter is handed the calling session's place in the supervision relation, read
/// at the call. A bound leaf is judged with the approver's, not the leaf holder's.
#[tokio::test]
async fn the_spotter_is_told_the_callers_supervision() {
    let (shared, _pointers) = me();
    let expected = Supervision {
        supervisor: Some("lead".into()),
        supervisees: vec![Supervisee {
            id: "kid".into(),
            created: true,
            status: ListedStatus::Idle,
        }],
    };
    let spotter = recording();
    let mut frontend = RecordingFrontend::answering(Approval::Abort);
    for (call, bound) in [
        (bash_call("curl -sI https://example.com"), None),
        (
            approve_call(),
            Some(bound_leaf("curl -sI https://example.com")),
        ),
    ] {
        judge(
            &mut frontend,
            &shared,
            &spotter,
            &Toolbox::bash_only(),
            1,
            &call,
            bound.as_ref(),
        )
        .await
        .unwrap();
    }
    let seen = spotter.seen.lock().unwrap();
    assert_eq!(seen.len(), 2);
    assert_eq!(seen[0].2.as_ref(), Some(&expected), "an ordinary move");
    assert_eq!(seen[1].2.as_ref(), Some(&expected), "a bound leaf");
}

/// A session with no host has no known relation, and the spotter is handed none, which
/// renders nothing rather than "no supervisor".
#[tokio::test]
async fn a_session_without_a_host_hands_the_spotter_no_supervision() {
    let shared = parent_with(crate::config::Config::default());
    let spotter = recording();
    judge(
        &mut RecordingFrontend::answering(Approval::Abort),
        &shared,
        &spotter,
        &Toolbox::bash_only(),
        1,
        &bash_call("curl -sI https://example.com"),
        None,
    )
    .await
    .unwrap();
    assert_eq!(spotter.seen.lock().unwrap()[0].2, None);
}
