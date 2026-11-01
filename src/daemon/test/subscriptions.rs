//! A session's status reaching its subscribers' inboxes (docs/tools.md).

use super::*;
use crate::daemon::Host;
use crate::protocol::turns::Landing;
use crate::record::log::Approval;
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
    async fn new() -> Self {
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
        adopt("parent", &project, None, false).await;
        adopt("peer", &project, None, false).await;
        adopt("child", &project, Some("parent"), false).await;
        adopt("grandchild", &project, Some("child"), false).await;
        adopt("loner", &project, Some("parent"), true).await;
        Self { root, host }
    }

    fn session(&self, id: &str) -> Arc<Session> {
        self.host.session(id).expect(id)
    }
}

/// `create` subscribes the creator with `wake`; any other subscription is ambient;
/// `unsubscribe` drops either. The subscriber's own `wait` source count follows.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_creator_is_subscribed_with_wake_and_a_peer_without() {
    let fixture = Fixture::new().await;
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

    assert!(fixture.host.subscribe("child", "peer", false));
    assert_eq!(
        subscribers("child"),
        vec![("parent".to_string(), true), ("peer".to_string(), false)]
    );
    assert!(fixture.session("peer").has_subscriptions());

    // An ambient subscription never lowers a creator's `wake`.
    assert!(fixture.host.subscribe("child", "parent", false));
    assert_eq!(
        subscribers("child"),
        vec![("parent".to_string(), true), ("peer".to_string(), false)]
    );

    assert!(fixture.host.unsubscribe("child", "peer"));
    assert!(!fixture.session("peer").has_subscriptions());
    assert!(
        !fixture.host.unsubscribe("child", "peer"),
        "nothing to drop"
    );

    fixture.host.swansong().shut_down().await;
}

/// Whether the parent's log holds a `Triage` that pinned `seq`.
fn pinned(session: &Session, seq: Seq) -> bool {
    session.snapshot().iter().any(|e| match &e.payload {
        Payload::Triage(t) => t
            .inbox
            .contains(&(seq, crate::record::log::Disposition::Pin)),
        _ => false,
    })
}

/// The child's gate reaches the parent as a must-wake notification on the
/// child's slot, and the parent's idle loop pins it even though its triage
/// cannot reach a model (the stub endpoint refuses connections); the resolution's
/// idle follows on the same slot. A sibling that did not subscribe hears nothing.
#[tokio::test(start_paused = true)]
async fn a_childs_gate_wakes_the_parent_and_is_pinned_whatever_the_triage_says() {
    let fixture = Fixture::new().await;
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
    let _rx = child.register_pending(&leaf, Default::default());
    let is_gate = |source: &str, body: &str, slot: Option<&str>| {
        source == "child"
            && slot == Some("session:child")
            && body.starts_with("waiting for your decision on fs.list")
            && body.contains("intent: locating siblings")
            && body.contains("decide: agents__decide(agent: \"child\"")
    };
    parent
        .wait_until("the gate reached the parent", || {
            heard(&parent, |n| is_gate(&n.source, &n.body, n.slot.as_deref())).is_some()
        })
        .await;
    assert!(fixture.session("peer").pending_notifications().is_empty());

    // The wake is declared, not judged: the triage fails outright here, and the
    // loop pins the status regardless (docs/inbox.md).
    parent
        .wait_until("the parent's idle triage pinned the triage", || {
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

    assert!(child.resolve_approval(
        &CallId("toolu_leaf".into()),
        Approval::Abort,
        Default::default()
    ));
    // The abort unwinds the turn, and the trajectory ends.
    child.going_idle();
    parent
        .wait_until("the idle reached the parent", || {
            heard(&parent, |n| {
                n.source == "child"
                    && n.slot.as_deref() == Some("session:child")
                    && n.body.starts_with("idle, its turn is done")
            })
            .is_some()
        })
        .await;

    fixture.host.swansong().shut_down().await;
}

/// A child's gate reaches its supervising creator in the reviewer's form: the held call
/// by its transcript handle, why it was held in plain words, and the calls that decide
/// it. The idle after it carries the child's whole closing message, past the length a
/// push caption keeps.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_supervisor_reads_the_gate_as_its_reviewer_and_the_whole_closing_message() {
    let fixture = Fixture::new().await;
    let parent = fixture.session("parent");
    let child = fixture.session("child");

    let curl = ToolCall::new(
        crate::record::names::tools::BASH,
        "toolu_curl",
        serde_json::json!({
            "command": "curl -sI https://example.com",
            "intent": "fetch only the response headers",
        }),
    );
    let issued = child
        .append(
            1,
            Payload::assistant(crate::record::log::AssistantOutput::from_legacy_messages(
                vec![],
                vec![],
                vec![curl.clone()],
            )),
        )
        .unwrap();
    child
        .append(
            1,
            Payload::Spot(
                crate::record::log::Spot::new(
                    curl.id.clone(),
                    crate::record::log::Stance::Gate,
                    "level 2 (scope fit, 21%): needs 1 approved call like it, found none.",
                )
                .with_axes(Some(crate::record::log::Axes {
                    in_scope: 0.21,
                    precedent: Some(crate::record::log::Precedent {
                        candidates: 0,
                        same: 0,
                        wider: 0,
                        required: Some(1),
                        compared: Vec::new(),
                    }),
                    ..Default::default()
                })),
            ),
        )
        .unwrap();
    let _rx = child.register_pending(&curl, Default::default());
    let gate = format!(
        "waiting for your decision on bash `curl -sI https://example.com` (seq \
         {issued})\nintent: fetch only the response headers\nheld because: its reviewer is \
         unsure this fits the session's scope; no earlier call like it was allowed\ndecide: \
         agents__decide(agent: \"child\", seq: \"{issued}\", decision: \"allow\" | \"deny\", \
         message?) — allow runs it, and a message rides with its result; deny with a message \
         refuses it and the session goes on under your message; a bare deny ends its \
         turn\nescalate: agents__decide(agent: \"child\", seq: \"{issued}\", decision: \
         \"escalate\", message?) — leaves it held and passes it to whoever decides above you, \
         with your message as a note to them"
    );
    parent
        .wait_until("the gate reached the parent as its reviewer", || {
            heard(&parent, |n| {
                n.source == "child" && n.slot.as_deref() == Some("session:child") && n.body == gate
            })
            .is_some()
        })
        .await;

    assert!(child.resolve_approval(&curl.id, Approval::Approve, Default::default()));
    let report = format!(
        "The headers came back.\n\n{}",
        "Each header is listed here in full, with what it says about the server. ".repeat(8)
    );
    let said = child
        .append(
            1,
            Payload::assistant(crate::record::log::AssistantOutput::from_legacy_messages(
                vec![],
                vec![ContentBlock::Text(report.clone())],
                vec![],
            )),
        )
        .unwrap();
    child.going_idle();
    let idle = format!(
        "idle, its turn is done (seq {said})\nsaid: {}\nnext: agents__message(agent: \
         \"child\", text: \"<message>\") sends its next message; agents__archive(agent: \
         \"child\") ends it",
        report.trim()
    );
    parent
        .wait_until("the idle reached the parent with the whole message", || {
            heard(&parent, |n| n.source == "child" && n.body == idle).is_some()
        })
        .await;

    fixture.host.swansong().shut_down().await;
}

/// A child's question reaches its parent as a must-wake status on the child's slot, with
/// the question and how to answer it; answering it moves the child on, and the parent
/// hears the next condition on the same slot.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_childs_question_wakes_the_parent() {
    let fixture = Fixture::new().await;
    let parent = fixture.session("parent");
    let child = fixture.session("child");
    assert!(fixture.session("peer").pending_notifications().is_empty());

    let mut wait = child.begin_ask("ask_1", "Which branch?".into(), 1, 1);
    parent
        .wait_until("the question reached the parent", || {
            heard(&parent, |n| {
                n.source == "child"
                    && n.slot.as_deref() == Some("session:child")
                    && n.body.starts_with("asking a question")
                    && n.body.contains("Which branch?")
                    && n.body
                        .contains("agents__message(agent: \"child\", text: \"<answer>\"")
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
    parent
        .wait_until("the parent heard the child move on", || {
            heard(&parent, |n| {
                n.source == "child"
                    && n.slot.as_deref() == Some("session:child")
                    && n.body.starts_with("idle, its turn is done")
            })
            .is_some()
        })
        .await;

    fixture.host.swansong().shut_down().await;
}

/// A question that returns a driver's queued message tells that driver, subscribed or not:
/// `parent` drives `grandchild` through the creator chain without a subscription, and is
/// woken with the status saying its message came back. `child`, subscribed, hears the
/// question without that line.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_question_tells_a_driver_its_message_came_back() {
    let fixture = Fixture::new().await;
    let parent = fixture.session("parent");
    let child = fixture.session("child");
    let grandchild = fixture.session("grandchild");
    // Held, so the idle loop leaves the message queued for the question to return.
    grandchild.set_pace(None, Some(true));
    let message =
        SubmittedTurn::new("try the other branch", "p1").sourced(Some("session[parent]".into()));
    assert_eq!(
        grandchild.submit_named_turn(message, None),
        Ok(Landing::Held)
    );
    let _wait = grandchild.begin_ask("ask_1", "Which branch?".into(), 1, 1);
    parent
        .wait_until("the parent heard its message came back", || {
            heard(&parent, |n| {
                n.source == "grandchild"
                    && n.body.contains(
                        "returned: your message \"try the other branch\" was returned undelivered",
                    )
            })
            .is_some()
        })
        .await;
    child
        .wait_until("the subscribed child heard the question", || {
            heard(&child, |n| {
                n.source == "grandchild"
                    && n.body.starts_with("asking a question")
                    && !n.body.contains("returned:")
            })
            .is_some()
        })
        .await;
    fixture.host.swansong().shut_down().await;
}

/// Ending a session posts its end to its subscribers, and ends the sessions it
/// commissioned — except a detached one, which outlives it.
#[tokio::test(start_paused = true)]
async fn an_end_is_posted_and_cascades_to_non_detached_children() {
    let fixture = Fixture::new().await;
    let parent = fixture.session("parent");
    let peer = fixture.session("peer");
    assert!(fixture.host.subscribe("child", "peer", false));
    assert!(peer.has_subscriptions());

    fixture.host.end("child").unwrap().await;
    fixture
        .host
        .wait_until("child gone from the host", || {
            fixture.host.session("child").is_none()
        })
        .await;
    fixture
        .host
        .wait_until("grandchild ended with its creator", || {
            fixture.host.session("grandchild").is_none()
        })
        .await;
    // The end is in the parent's queue or, once its live loop has drained the inbox after
    // the idle debounce, an `Ambient` entry: `told` reads both. The assertions below use
    // what the wait read, since a later read could meet the drain mid-way.
    let mut heard_by_then = Vec::new();
    parent
        .wait_until("the parent heard the end", || {
            heard_by_then = told(&parent);
            heard_by_then.iter().any(|n| n.body.starts_with("ended"))
        })
        .await;
    assert_eq!(heard_by_then.len(), 1);
    assert_eq!(heard_by_then[0].slot.as_deref(), Some("session:child"));
    assert!(heard_by_then[0].wake);
    // The ended session can wake nobody: its subscribers' `wait` sources drop.
    assert!(!peer.has_subscriptions());
    assert!(parent.has_subscriptions(), "loner is still the parent's");
    assert!(
        fixture.host.session("loner").is_some(),
        "a detached child is not the parent's to end — and child never created it"
    );

    fixture.host.end("parent").unwrap().await;
    // The end resolves once the parent and the sessions that end with it have left
    // (docs/control-channel.md), so one still hosted now outlives it.
    assert!(fixture.host.session("parent").is_none());
    assert!(
        fixture.host.session("loner").is_some(),
        "detached: outlives its creator"
    );
    assert!(fixture.host.session("peer").is_some());

    fixture.host.swansong().shut_down().await;
}
