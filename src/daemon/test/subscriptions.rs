//! A session's status reaching its subscribers' inboxes (docs/tools.md).

use super::*;
use crate::control_tools::{Control, Executor, Subscribe, Unsubscribe};
use crate::daemon::{Host, SessionRuntime};
use crate::frontend::Approval;
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
    /// One project: `parent` commissioned `child` and, detached, `loner`;
    /// `child` commissioned `grandchild`; `peer` is a human root.
    fn new() -> Self {
        use std::hash::{BuildHasher, Hasher, RandomState};
        let nonce = RandomState::new().build_hasher().finish();
        let root = std::env::temp_dir().join(format!("efference-subscriptions-{nonce:016x}"));
        let root = root.canonicalize().unwrap_or(root);
        let project = root.join("a");
        let ids = ["parent", "peer", "child", "grandchild", "loner"];
        for id in ids {
            std::fs::create_dir_all(project.join(".efference").join(id)).unwrap();
        }
        let host = Host::new(crate::inference::InferenceRegistry::empty(), baseline_env());
        let adopt = |id: &str, cwd: &Path, created_by: Option<&str>, detached: bool| {
            let boot = runtime::boot_in(cwd, &cwd.join(".efference").join(id));
            host.adopt_with(
                id.to_string(),
                SessionRuntime::start(boot, host.swansong()),
                created_by.map(str::to_string),
                detached,
            )
            .expect("the id is free in this test");
        };
        adopt("parent", &project, None, false);
        adopt("peer", &project, None, false);
        adopt("child", &project, Some("parent"), false);
        adopt("grandchild", &project, Some("child"), false);
        adopt("loner", &project, Some("parent"), true);
        Self { root, host }
    }

    fn control(&self, caller: &str) -> Control {
        Control::new(
            Executor::host(Arc::clone(&self.host), caller.to_string()),
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

/// `create` subscribes the creator with `wake`; an explicit `subscribe` is
/// ambient; `unsubscribe` drops either. The subscriber's own `wait` source
/// count follows.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_creator_is_subscribed_with_wake_and_a_peer_without() {
    let fixture = Fixture::new();
    let subscribers = |id: &str| {
        fixture
            .host
            .subscribers(id)
            .into_iter()
            .map(|(id, terms)| (id, terms.wake))
            .collect::<Vec<_>>()
    };
    assert_eq!(subscribers("child"), vec![("parent".to_string(), true)]);
    assert_eq!(subscribers("loner"), vec![("parent".to_string(), true)]);
    assert!(subscribers("peer").is_empty());
    assert!(fixture.session("parent").has_subscriptions());
    assert!(!fixture.session("peer").has_subscriptions());

    let peer = fixture.control("peer");
    Subscribe {
        session: "child".into(),
    }
    .execute(peer.clone(), RequestContext::default())
    .await
    .expect("a peer may observe");
    assert_eq!(
        subscribers("child"),
        vec![("parent".to_string(), true), ("peer".to_string(), false)]
    );
    assert!(fixture.session("peer").has_subscriptions());

    // An explicit `subscribe` never lowers a creator's `wake`.
    let parent = fixture.control("parent");
    Subscribe {
        session: "child".into(),
    }
    .execute(parent.clone(), RequestContext::default())
    .await
    .unwrap();
    assert_eq!(
        subscribers("child"),
        vec![("parent".to_string(), true), ("peer".to_string(), false)]
    );

    let err = Subscribe {
        session: "peer".into(),
    }
    .execute(peer.clone(), RequestContext::default())
    .await
    .unwrap_err()
    .to_string();
    assert!(err.contains("cannot target itself"), "{err}");

    let said = Unsubscribe {
        session: "child".into(),
    }
    .execute(peer.clone(), RequestContext::default())
    .await
    .unwrap();
    assert_eq!(said, "unsubscribed from child");
    assert!(!fixture.session("peer").has_subscriptions());
    let said = Unsubscribe {
        session: "child".into(),
    }
    .execute(peer.clone(), RequestContext::default())
    .await
    .unwrap();
    assert_eq!(said, "you were not subscribed to child");

    fixture.host.swansong().shut_down().await;
}

/// Where a notification to `session` is now: still pending, or logged at its
/// first show as an `Ambient` (the parent's live loop drains its inbox after the
/// idle debounce, so a test cannot count on catching it pending).
fn heard(session: &Session, matches: impl Fn(&str, &str, Option<&str>) -> bool) -> Option<Seq> {
    if let Some(n) = session
        .pending_notifications()
        .iter()
        .find(|n| matches(&n.source, &n.body, n.slot.as_deref()))
    {
        return Some(u64::MAX - n.id);
    }
    session.snapshot().iter().find_map(|e| match &e.payload {
        Payload::Ambient(n) if matches(&n.source, &n.body, n.slot.as_deref()) => Some(e.seq),
        _ => None,
    })
}

/// Whether the parent's log holds a `Triage` that pinned `seq`.
fn pinned(session: &Session, seq: Seq) -> bool {
    session.snapshot().iter().any(|e| match &e.payload {
        Payload::Triage(t) => t.inbox.contains(&(seq, crate::log::Disposition::Pin)),
        _ => false,
    })
}

/// The child's gate reaches the parent as a must-wake notification on the
/// child's slot, and the parent's idle loop pins it even though its triage
/// cannot reach a model (the stub endpoint refuses connections); the resolution's
/// idle follows on the same slot. A sibling that did not subscribe hears nothing.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_childs_gate_wakes_the_parent_and_is_pinned_whatever_the_triage_says() {
    let fixture = Fixture::new();
    let parent = fixture.session("parent");
    let child = fixture.session("child");
    assert!(
        parent.pending_notifications().is_empty(),
        "boot idle is not news"
    );

    let leaf = ToolCall::new(
        "fs__list",
        "toolu_leaf",
        serde_json::json!({ "path": "/Users/x/code", "intent": "locating siblings" }),
    );
    let _rx = child.register_pending(&leaf, None);
    let is_gate = |source: &str, body: &str, slot: Option<&str>| {
        source == "child"
            && slot == Some("session:child")
            && body.starts_with("child is gated on fs.list — locating siblings")
            && body.contains("call_id toolu_leaf")
    };
    wait_until("the gate reached the parent", || {
        heard(&parent, is_gate).is_some()
    })
    .await;
    assert!(fixture.session("peer").pending_notifications().is_empty());

    // The wake is declared, not judged: the triage fails outright here, and the
    // loop pins the status regardless (docs/inbox.md).
    wait_until("the parent's idle triage pinned the triage", || {
        parent
            .snapshot()
            .iter()
            .find_map(|e| match &e.payload {
                Payload::Ambient(n) if is_gate(&n.source, &n.body, n.slot.as_deref()) => {
                    Some(e.seq)
                }
                _ => None,
            })
            .is_some_and(|seq| pinned(&parent, seq))
    })
    .await;

    assert!(child.resolve_approval(&CallId("toolu_leaf".into()), Approval::Abort));
    // The abort unwinds the turn, and the trajectory ends.
    child.going_idle();
    wait_until("the idle reached the parent", || {
        heard(&parent, |source, body, slot| {
            source == "child"
                && slot == Some("session:child")
                && body.starts_with("child finished its turn and is idle")
        })
        .is_some()
    })
    .await;

    fixture.host.swansong().shut_down().await;
}

/// A child's question reaches its parent as a must-wake status on the child's slot, with
/// the question and how to answer it; answering it moves the child on, and the parent
/// hears the next condition on the same slot.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_childs_question_wakes_the_parent() {
    let fixture = Fixture::new();
    let parent = fixture.session("parent");
    let child = fixture.session("child");
    assert!(fixture.session("peer").pending_notifications().is_empty());

    let mut wait = child.begin_ask("ask_1", "Which branch?".into(), 1, 1);
    wait_until("the question reached the parent", || {
        heard(&parent, |source, body, slot| {
            source == "child"
                && slot == Some("session:child")
                && body.starts_with("child is asking a question")
                && body.contains("Which branch?")
                && body.contains("sessions__turn")
        })
        .is_some()
    })
    .await;
    assert!(fixture.session("peer").pending_notifications().is_empty());

    // Take the reply as the loop does: dropping an unread reply resubmits it as a
    // turn, which would race the idle this waits for.
    child.submit_turn(SubmittedTurn::new("main", "s1"));
    assert!(wait.reply().await.is_some());
    drop(wait);
    // The loop takes the reply as the call's result and its trajectory ends.
    child.going_idle();
    wait_until("the parent heard the child move on", || {
        heard(&parent, |source, body, slot| {
            source == "child"
                && slot == Some("session:child")
                && body.starts_with("child finished its turn and is idle")
        })
        .is_some()
    })
    .await;

    fixture.host.swansong().shut_down().await;
}

/// Ending a session posts its end to its subscribers, and ends the sessions it
/// commissioned — except a detached one, which outlives it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_end_is_posted_and_cascades_to_non_detached_children() {
    let fixture = Fixture::new();
    let parent = fixture.session("parent");
    let peer = fixture.session("peer");
    assert!(fixture.host.subscribe("child", "peer", false));
    assert!(peer.has_subscriptions());

    fixture.host.end("child").unwrap().await;
    wait_until("child gone from the host", || {
        fixture.host.session("child").is_none()
    })
    .await;
    wait_until("grandchild ended with its creator", || {
        fixture.host.session("grandchild").is_none()
    })
    .await;
    wait_until("the parent heard the end", || {
        parent
            .pending_notifications()
            .iter()
            .any(|n| n.body.starts_with("child ended"))
    })
    .await;
    let pending = parent.pending_notifications();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].slot.as_deref(), Some("session:child"));
    assert!(pending[0].wake);
    // The ended session can wake nobody: its subscribers' `wait` sources drop.
    assert!(!peer.has_subscriptions());
    assert!(parent.has_subscriptions(), "loner is still the parent's");
    assert!(
        fixture.host.session("loner").is_some(),
        "a detached child is not the parent's to end — and child never created it"
    );

    fixture.host.end("parent").unwrap().await;
    wait_until("parent gone", || fixture.host.session("parent").is_none()).await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(
        fixture.host.session("loner").is_some(),
        "detached: outlives its creator"
    );
    assert!(fixture.host.session("peer").is_some());

    fixture.host.swansong().shut_down().await;
}
