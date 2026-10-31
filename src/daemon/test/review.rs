//! The `supervisor` review position: who answers a gate, and when that is decided
//! (docs/permissions.md).

use super::*;
use crate::control_tools::{Control, Decide, Decision, Executor};
use crate::daemon::Host;
use crate::policy::ReviewLevel;
use mcplease::traits::Tool as _;
use mcplease::types::RequestContext;
use std::path::Path;

struct Fixture {
    root: PathBuf,
    host: Arc<Host>,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

impl Fixture {
    /// One project: `root` is a human root; `child` and the detached `outliver`
    /// were both commissioned by it.
    async fn new() -> Self {
        use std::hash::{BuildHasher, Hasher, RandomState};
        let nonce = RandomState::new().build_hasher().finish();
        let dir = std::env::temp_dir().join(format!("efference-review-{nonce:016x}"));
        let dir = dir.canonicalize().unwrap_or(dir);
        let project = dir.join("a");
        for id in ["root", "child", "outliver"] {
            std::fs::create_dir_all(project.join(".efference").join(id)).unwrap();
        }
        let host = Host::new(crate::inference::InferenceRegistry::empty(), baseline_env());
        let adopt = async |id: &str, cwd: &Path, created_by: Option<&str>, detached: bool| {
            let boot = runtime::boot_in(cwd, &cwd.join(".efference").join(id));
            host.adopt_with(
                id.to_string(),
                runtime::started(boot, host.swansong()).await,
                created_by.map(str::to_string),
                detached,
            )
            .expect("the id is free in this test");
        };
        adopt("root", &project, None, false).await;
        adopt("child", &project, Some("root"), false).await;
        adopt("outliver", &project, Some("root"), true).await;
        Self { root: dir, host }
    }

    fn control(&self, caller: &str) -> Control {
        Control::new(Executor::host(&self.host, caller.to_string()), false)
    }

    fn session(&self, id: &str) -> Arc<Session> {
        self.host.session(id).expect(id)
    }
}

async fn wait_until(what: &str, mut ready: impl FnMut() -> bool) {
    tokio::time::timeout(Duration::from_secs(5), async {
        while !ready() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("timed out waiting: {what}"));
}

/// Whether `session` has heard a notification whose body matches — pending, or
/// already logged as an `Ambient` by its live loop's first show.
fn heard(session: &Session, matches: impl Fn(&str) -> bool) -> bool {
    session
        .pending_notifications()
        .iter()
        .any(|n| matches(&n.body))
        || session.snapshot().iter().any(|e| match &e.payload {
            Payload::Ambient(n) => matches(&n.body),
            _ => false,
        })
}

fn leaf() -> ToolCall {
    ToolCall::new(
        "fs__list",
        "toolu_leaf",
        serde_json::json!({ "path": "/Users/x/code", "intent": "locating siblings" }),
    )
}

/// A session another session commissioned answers to it; a human root has no
/// supervisor to answer to and stays on the supervised default.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_commissioned_session_defaults_to_supervisor_and_a_root_does_not() {
    let fixture = Fixture::new().await;

    assert_eq!(
        fixture.session("root").review_level(),
        ReviewLevel::SpotterHuman
    );
    assert_eq!(fixture.session("root").supervisor(), None);
    assert_eq!(
        fixture.session("root").resolved_review_level(),
        ReviewLevel::SpotterHuman
    );

    for child in ["child", "outliver"] {
        assert_eq!(
            fixture.session(child).review_level(),
            ReviewLevel::Supervisor,
            "{child}"
        );
        assert_eq!(fixture.session(child).supervisor().as_deref(), Some("root"));
        assert_eq!(
            fixture.session(child).resolved_review_level(),
            ReviewLevel::Supervisor,
            "{child}"
        );
    }

    fixture.host.swansong().shut_down().await;
}

/// The position is resolved **when a call is routed**, never once at
/// creation. `outliver` is commissioned by `root` and outlives it; while `root`
/// runs, its gate is `root`'s to answer, and the instant `root` ends the same
/// unchanged dial reviews as `spotter_human` — the operator — rather than leaving
/// a held call with nobody to answer it. Resolving at creation is exactly what
/// this proves is not happening: the dial itself never moves.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn supervisor_resolves_when_a_call_is_routed_not_when_the_session_is_created() {
    let fixture = Fixture::new().await;
    let root = fixture.session("root");
    let outliver = fixture.session("outliver");

    let state: Value = serde_json::from_str(&outliver.state_json()).unwrap();
    assert_eq!(state["supervisor"], "root", "the topic names the creator");

    let _gate = outliver.register_pending(&leaf(), None, None);
    wait_until("the gate reached its supervisor", || {
        heard(&root, |body| body.contains("waiting for your decision"))
    })
    .await;
    assert_eq!(outliver.resolved_review_level(), ReviewLevel::Supervisor);

    // The creator ends. `outliver` is detached, so it keeps running — and keeps
    // holding the call.
    fixture.host.end("root").unwrap().await;
    wait_until("the creator left the host", || {
        fixture.host.session("root").is_none()
    })
    .await;

    assert_eq!(
        outliver.review_level(),
        ReviewLevel::Supervisor,
        "the dial is untouched — only what it resolves to moved"
    );
    assert_eq!(outliver.supervisor(), None);
    assert_eq!(
        outliver.resolved_review_level(),
        ReviewLevel::SpotterHuman,
        "an unresolvable position moves toward the operator, never toward autonomous"
    );
    let state: Value = serde_json::from_str(&outliver.state_json()).unwrap();
    assert_eq!(state["review_level"], "supervisor");
    assert_eq!(state["resolved_review_level"], "spotter_human");
    assert!(
        state["supervisor"].is_null(),
        "no live supervisor reads null"
    );
    assert!(
        !state["gate"].is_null(),
        "the held call is still held, for the operator to answer"
    );

    fixture.host.swansong().shut_down().await;
}

/// Turning the dial while a call is held **re-routes** that gate; it does not drop
/// it. Handing future approvals to someone else is not abandoning a call that is
/// holding a child right now, and taking them on is not waiting for the next one.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_level_change_reroutes_a_pending_gate() {
    let fixture = Fixture::new().await;
    let root = fixture.session("root");
    let child = fixture.session("child");

    // Start with the operator holding the review, so the creator — still the
    // supervisor, which may decide the gate — is not its reviewer.
    child.set_review_level(ReviewLevel::SpotterHuman);
    let _gate = child.register_pending(&leaf(), None, None);
    wait_until("the creator observed the gate", || {
        heard(&root, |body| {
            body.starts_with("waiting for a decision from the operator or you on fs.list")
        })
    })
    .await;

    // The operator's dial moves toward the creator with the call still held.
    child.set_review_level(ReviewLevel::Supervisor);

    wait_until("the held call was re-routed to the creator", || {
        heard(&root, |body| {
            body.contains("waiting for your decision") && body.contains("fs.list")
        })
    })
    .await;
    assert!(
        child.pending_gate().is_some(),
        "re-routed, not dropped — the same call is still held"
    );

    // …and back: handing the child to the operator leaves the call held too.
    child.set_review_level(ReviewLevel::SpotterHuman);
    assert!(child.pending_gate().is_some());
    assert_eq!(child.resolved_review_level(), ReviewLevel::SpotterHuman);

    fixture.host.swansong().shut_down().await;
}

/// The routed gate resolves through the ordinary `decide` — the position adds a
/// recipient, not a verb (docs/permissions.md).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_reviewer_resolves_the_routed_gate_through_decide() {
    let fixture = Fixture::new().await;
    let root = fixture.session("root");
    let child = fixture.session("child");

    let issued = child
        .append(
            1,
            Payload::Assistant {
                output: crate::log::AssistantOutput::from_legacy_messages(
                    vec![],
                    vec![],
                    vec![leaf()],
                ),
                excluded: None,
            },
        )
        .unwrap();
    let gate = child.register_pending(&leaf(), None, None);
    wait_until("the gate reached its reviewer", || {
        heard(&root, |body| body.contains("waiting for your decision"))
    })
    .await;

    let reviewer = fixture.control("root");
    Decide {
        agent: "child".into(),
        seq: issued.to_string(),
        decision: Decision::Allow,
        message: None,
    }
    .execute(reviewer.clone(), RequestContext::default())
    .await
    .expect("the creator drives the child, so its decision reaches it");

    assert!(matches!(gate.await, Ok(Approval::Approve)));
    assert!(child.pending_gate().is_none());

    fixture.host.swansong().shut_down().await;
}

/// The gate reaches the creator on the dial alone: `unsubscribe` drops the
/// observation and the routing survives it, which is the distinction the position
/// exists to draw (docs/inbox.md).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_unsubscribed_creator_still_receives_the_gate_it_reviews() {
    let fixture = Fixture::new().await;
    let root = fixture.session("root");
    let child = fixture.session("child");

    assert!(fixture.host.unsubscribe("child", "root"));
    assert!(fixture.host.subscribers("child").is_empty());

    let _gate = child.register_pending(&leaf(), None, None);
    wait_until("the gate reached its reviewer regardless", || {
        heard(&root, |body| body.contains("waiting for your decision"))
    })
    .await;

    fixture.host.swansong().shut_down().await;
}
