//! The `upstream` review position: who answers a gate, and when that is decided
//! (docs/permissions.md).

use super::*;
use crate::control_tools::{Control, Executor, ReviewLevel as ReviewLevelTool};
use crate::daemon::{Host, SessionRuntime};
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
    fn new() -> Self {
        use std::hash::{BuildHasher, Hasher, RandomState};
        let nonce = RandomState::new().build_hasher().finish();
        let dir = std::env::temp_dir().join(format!("efference-review-{nonce:016x}"));
        let dir = dir.canonicalize().unwrap_or(dir);
        let project = dir.join("a");
        for id in ["root", "child", "outliver"] {
            std::fs::create_dir_all(project.join(".efference").join(id)).unwrap();
        }
        let host = Host::new(crate::inference::InferenceRegistry::empty());
        let adopt = |id: &str, cwd: &Path, created_by: Option<&str>, detached: bool| {
            let boot = runtime::boot_in(cwd, &cwd.join(".efference").join(id));
            host.adopt_with(
                id.to_string(),
                SessionRuntime::start(boot, host.swansong()),
                created_by.map(str::to_string),
                detached,
            );
        };
        adopt("root", &project, None, false);
        adopt("child", &project, Some("root"), false);
        adopt("outliver", &project, Some("root"), true);
        Self { root: dir, host }
    }

    fn control(&self, caller: &str) -> Control {
        Control::new(
            Executor::host(
                tokio::runtime::Handle::current(),
                Arc::clone(&self.host),
                caller.to_string(),
            ),
            false,
        )
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
/// upstream to answer to and stays on the supervised default.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_commissioned_session_defaults_to_upstream_and_a_root_does_not() {
    let fixture = Fixture::new();

    assert_eq!(
        fixture.session("root").review_level(),
        ReviewLevel::SpotterHuman
    );
    assert_eq!(fixture.session("root").upstream(), None);
    assert_eq!(
        fixture.session("root").resolved_review_level(),
        ReviewLevel::SpotterHuman
    );

    for child in ["child", "outliver"] {
        assert_eq!(
            fixture.session(child).review_level(),
            ReviewLevel::Upstream,
            "{child}"
        );
        assert_eq!(fixture.session(child).upstream().as_deref(), Some("root"));
        assert_eq!(
            fixture.session(child).resolved_review_level(),
            ReviewLevel::Upstream,
            "{child}"
        );
    }

    fixture.host.swansong().shut_down().await;
}

/// The position is resolved **at the moment a call is routed**, never once at
/// creation. `outliver` is commissioned by `root` and outlives it; while `root`
/// runs, its gate is `root`'s to answer, and the instant `root` ends the same
/// unchanged dial reviews as `spotter_human` — the operator — rather than leaving
/// a held call with nobody to answer it. Resolving at creation is exactly what
/// this proves is not happening: the dial itself never moves.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn upstream_resolves_when_a_call_is_routed_not_when_the_session_is_created() {
    let fixture = Fixture::new();
    let root = fixture.session("root");
    let outliver = fixture.session("outliver");

    let _gate = outliver.register_pending(&leaf(), None);
    wait_until("the gate reached its upstream reviewer", || {
        heard(&root, |body| body.contains("You are its upstream reviewer"))
    })
    .await;
    assert_eq!(outliver.resolved_review_level(), ReviewLevel::Upstream);

    // The creator ends. `outliver` is detached, so it keeps running — and keeps
    // holding the call.
    fixture.host.end("root").unwrap().await;
    wait_until("the creator left the host", || {
        fixture.host.session("root").is_none()
    })
    .await;

    assert_eq!(
        outliver.review_level(),
        ReviewLevel::Upstream,
        "the dial is untouched — only what it resolves to moved"
    );
    assert_eq!(outliver.upstream(), None);
    assert_eq!(
        outliver.resolved_review_level(),
        ReviewLevel::SpotterHuman,
        "an unresolvable position moves toward the operator, never toward autonomous"
    );
    let state: Value = serde_json::from_str(&outliver.state_json()).unwrap();
    assert_eq!(state["review_level"], "upstream");
    assert_eq!(state["resolved_review_level"], "spotter_human");
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
    let fixture = Fixture::new();
    let root = fixture.session("root");
    let child = fixture.session("child");

    // Start with the operator holding the review, so the creator is only an
    // observer of the gate rather than its reviewer.
    child.set_review_level(ReviewLevel::SpotterHuman);
    let _gate = child.register_pending(&leaf(), None);
    wait_until("the creator observed the gate", || {
        heard(&root, |body| body.contains("child is gated on fs.list"))
    })
    .await;

    // The dial moves toward the creator with the call still held.
    let mut operator = fixture.control("root");
    ReviewLevelTool {
        level: "upstream".into(),
        session: Some("child".into()),
    }
    .execute(&mut operator, &RequestContext::default())
    .unwrap();

    wait_until("the held call was re-routed to the creator", || {
        heard(&root, |body| {
            body.contains("You are its upstream reviewer") && body.contains("toolu_leaf")
        })
    })
    .await;
    assert!(
        child.pending_gate().is_some(),
        "re-routed, not dropped — the same call is still held"
    );

    // …and back: handing the child to the operator leaves the call held too.
    ReviewLevelTool {
        level: "spotter_human".into(),
        session: Some("child".into()),
    }
    .execute(&mut operator, &RequestContext::default())
    .unwrap();
    assert!(child.pending_gate().is_some());
    assert_eq!(child.resolved_review_level(), ReviewLevel::SpotterHuman);

    fixture.host.swansong().shut_down().await;
}

/// The routed gate resolves through the ordinary `approve` route — the position
/// adds a recipient, not a verb (docs/permissions.md).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_reviewer_resolves_the_routed_gate_through_approve() {
    let fixture = Fixture::new();
    let root = fixture.session("root");
    let child = fixture.session("child");

    let gate = child.register_pending(&leaf(), None);
    wait_until("the gate reached its reviewer", || {
        heard(&root, |body| body.contains("You are its upstream reviewer"))
    })
    .await;

    let mut reviewer = fixture.control("root");
    crate::control_tools::Approve {
        call_id: Some("toolu_leaf".into()),
        abort: false,
        redirect: None,
        wait: false,
        timeout_s: None,
        session: Some("child".into()),
    }
    .execute(&mut reviewer, &RequestContext::default())
    .expect("the creator is an ancestor, so the mutating verb reaches the child");

    assert!(matches!(gate.await, Ok(Approval::Approve)));
    assert!(child.pending_gate().is_none());

    fixture.host.swansong().shut_down().await;
}

/// The gate reaches the creator on the dial alone: `unsubscribe` drops the
/// observation and the routing survives it, which is the distinction the position
/// exists to draw (docs/inbox.md).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_unsubscribed_creator_still_receives_the_gate_it_reviews() {
    let fixture = Fixture::new();
    let root = fixture.session("root");
    let child = fixture.session("child");

    assert!(fixture.host.unsubscribe("child", "root"));
    assert!(fixture.host.subscribers("child").is_empty());

    let _gate = child.register_pending(&leaf(), None);
    wait_until("the gate reached its reviewer regardless", || {
        heard(&root, |body| body.contains("You are its upstream reviewer"))
    })
    .await;

    fixture.host.swansong().shut_down().await;
}
