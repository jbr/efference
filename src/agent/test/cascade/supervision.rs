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

/// An `agents__message` the calling session writes, to `target`.
fn message(target: &str) -> ToolCall {
    ToolCall::new(
        "agents__message",
        "toolu_message",
        serde_json::json!({ "agent": target, "text": "status?" }),
    )
}

/// A child's held `agents__message`, bound to the parent's approval.
fn bound_message() -> crate::protocol::agents::BoundAction {
    crate::protocol::agents::BoundAction {
        name: "agents__message".into(),
        input: serde_json::json!({ "agent": "grandchild", "text": "go on" }),
        ..bound_leaf("unused")
    }
}

/// Judge each of `moves` as `shared`'s, with the session-control verbs mounted, and
/// return what the spotter was handed for each.
async fn seen_for(
    shared: &Arc<Session>,
    moves: Vec<(ToolCall, Option<crate::protocol::agents::BoundAction>)>,
) -> Vec<Seen> {
    let spotter = recording();
    let mut frontend = RecordingFrontend::answering(Approval::Abort);
    let toolbox = with_agent_verbs("me");
    for (call, bound) in moves {
        judge(
            &mut frontend,
            shared,
            &spotter,
            &toolbox,
            1,
            &call,
            bound.as_ref(),
        )
        .await
        .unwrap();
    }
    spotter.seen.into_inner().unwrap()
}

/// A session-control move is handed the calling session's place in the supervision
/// relation, read at the call. A bound leaf that is one is judged with the approver's
/// place, not the leaf holder's.
#[tokio::test]
async fn a_session_control_move_is_told_the_callers_supervision() {
    let (shared, _pointers) = me();
    let expected = Supervision {
        supervisor: Some("lead".into()),
        supervisees: vec![Supervisee {
            id: "kid".into(),
            created: true,
            status: ListedStatus::Idle,
        }],
    };
    let seen = seen_for(
        &shared,
        vec![
            (message("elsewhere"), None),
            (approve_call(), Some(bound_message())),
        ],
    )
    .await;
    assert_eq!(seen.len(), 2);
    assert_eq!(seen[0].2.as_ref(), Some(&expected), "an ordinary move");
    assert_eq!(seen[1].2.as_ref(), Some(&expected), "a bound leaf");
}

/// Any other move — a shell command here, held by this session or bound to its approval —
/// is handed no supervision facts at all (docs/permissions.md).
#[tokio::test]
async fn any_other_move_is_told_no_supervision() {
    let (shared, _pointers) = me();
    let seen = seen_for(
        &shared,
        vec![
            (bash_call("curl -sI https://example.com"), None),
            (
                approve_call(),
                Some(bound_leaf("curl -sI https://example.com")),
            ),
        ],
    )
    .await;
    assert_eq!(seen.len(), 2);
    assert_eq!(seen[0].2, None, "an ordinary move");
    assert_eq!(seen[1].2, None, "a bound leaf");
}

/// A session with no host has no known relation, and the spotter is handed none, which
/// renders nothing rather than "no supervisor".
#[tokio::test]
async fn a_session_without_a_host_hands_the_spotter_no_supervision() {
    let shared = parent_with(crate::config::Config::default());
    let seen = seen_for(&shared, vec![(message("elsewhere"), None)]).await;
    assert_eq!(seen[0].2, None);
}

/// The spotter is handed what was said in the calling session — for a bound leaf, the
/// approver's conversation.
#[tokio::test]
async fn the_spotter_is_handed_the_callers_utterances() {
    let (shared, _pointers) = me();
    shared
        .append_submission(
            1,
            Payload::user("open the gallery in Playwright"),
            "sub-1".into(),
            None,
        )
        .unwrap();
    let seen = seen_for(
        &shared,
        vec![
            (bash_call("curl -sI https://example.com"), None),
            (
                approve_call(),
                Some(bound_leaf("curl -sI https://example.com")),
            ),
        ],
    )
    .await;
    let expected = vec![crate::record::utterances::Utterance::new(
        crate::record::utterances::Speaker::Operator,
        "open the gallery in Playwright",
    )];
    assert_eq!(seen[0].3.as_ref(), Some(&expected), "an ordinary move");
    assert_eq!(seen[1].3.as_ref(), Some(&expected), "a bound leaf");
}
