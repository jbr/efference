//! Which session a moment goes to, and so whether the operator is pushed
//! (docs/web-push.md): a supervised session's moments go to its supervisor, a gate goes
//! to its reviewer, and a gate that comes to rest with the operator later is pushed then.

use super::*;
use crate::daemon::Host;
use crate::policy::ReviewLevel;
use crate::protocol::state::LoopPhase;
use crate::record::log::{Approval, MomentKind, PushEvent, PushRecord, Telemetry};

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
    /// One project: `boss` is a root, which created `kid` and, detached, `stray` and
    /// `mid`; `mid` created `leaf`, detached — so `leaf → mid → boss` is a chain that
    /// survives `boss` ending. `solo` is a root nobody supervises.
    async fn new() -> Self {
        use std::hash::{BuildHasher, Hasher, RandomState};
        let nonce = RandomState::new().build_hasher().finish();
        let root = std::env::temp_dir().join(format!("efference-push-routing-{nonce:016x}"));
        let root = root.canonicalize().unwrap_or(root);
        let project = root.join("one");
        for id in ["boss", "kid", "stray", "mid", "leaf", "solo"] {
            std::fs::create_dir_all(project.join(".efference").join(id)).unwrap();
        }
        let host = Host::new(crate::inference::InferenceRegistry::empty(), baseline_env());
        let adopt = async |id: &str, created_by: Option<&str>, detached: bool| {
            let boot = runtime::boot_in(&project, &project.join(".efference").join(id));
            host.adopt_with(
                id.to_string(),
                runtime::started(boot, host.swansong()).await,
                created_by.map(str::to_string),
                detached,
            )
            .expect("the id is free in this test");
        };
        adopt("boss", None, false).await;
        adopt("kid", Some("boss"), false).await;
        adopt("stray", Some("boss"), true).await;
        adopt("mid", Some("boss"), true).await;
        adopt("leaf", Some("mid"), true).await;
        adopt("solo", None, false).await;
        Self { root, host }
    }

    fn session(&self, id: &str) -> Arc<Session> {
        self.host.session(id).expect(id)
    }
}

/// What `session` sends from here on, in place of the push services.
fn capture(session: &Session) -> Arc<Mutex<Vec<crate::platform::push::Message>>> {
    let sent = Arc::new(Mutex::new(Vec::new()));
    let into = Arc::clone(&sent);
    session.set_push_transport(Arc::new(move |message| {
        into.lock().unwrap().push(message);
        Box::pin(async { Vec::new() })
    }));
    sent
}

/// `session`'s push decisions, in order, without the push services' answers.
fn decisions(session: &Session) -> Vec<(MomentKind, PushEvent)> {
    session
        .snapshot()
        .into_iter()
        .filter_map(|e| match e.payload {
            Payload::Telemetry(Telemetry::Push(PushRecord { kind, event, .. })) => {
                Some((kind, event))
            }
            _ => None,
        })
        .filter(|(_, event)| !matches!(event, PushEvent::Delivery { .. }))
        .collect()
}

fn not_sent(kind: MomentKind, to: &str) -> (MomentKind, PushEvent) {
    (kind, PushEvent::NotSent { to: to.into() })
}

fn curl(id: &str) -> ToolCall {
    ToolCall::new(
        crate::record::names::tools::BASH,
        id,
        serde_json::json!({
            "command": "curl -sI https://example.com",
            "intent": "fetch only the response headers",
        }),
    )
}

/// A turn ends and the loop goes idle, as the loop does it.
fn settle(session: &Session) {
    session.set_phase(LoopPhase::Requesting);
    session.going_idle();
}

/// A supervised child's gate, `ask`, and settle each go to its supervisor, which hears
/// each through its subscription; none of them pushes, and each is recorded as not sent.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_supervised_sessions_moments_push_nothing() {
    let fixture = Fixture::new().await;
    let (boss, kid) = (fixture.session("boss"), fixture.session("kid"));
    let sent = capture(&kid);

    let _gate = kid.register_pending(&curl("toolu_1"), Default::default());
    boss.wait_until("the gate reached boss", || {
        heard(&boss, |n| {
            n.source == "kid" && n.body.starts_with("waiting for your decision")
        })
        .is_some()
    })
    .await;
    assert!(kid.resolve_approval(&"toolu_1".into(), Approval::Approve, Default::default()));

    let wait = kid.begin_ask("ask_1", "Which branch?".into(), 1, 1);
    boss.wait_until("the question reached boss", || {
        heard(&boss, |n| {
            n.source == "kid" && n.body.starts_with("asking a question")
        })
        .is_some()
    })
    .await;
    drop(wait);

    settle(&kid);
    boss.wait_until("the idle reached boss", || {
        heard(&boss, |n| {
            n.source == "kid" && n.body.starts_with("idle, its turn is done")
        })
        .is_some()
    })
    .await;

    assert_eq!(
        decisions(&kid),
        [
            not_sent(MomentKind::Gate, "boss"),
            not_sent(MomentKind::Ask, "boss"),
            not_sent(MomentKind::Settle, "boss"),
        ]
    );
    assert!(sent.lock().unwrap().is_empty());

    fixture.host.swansong().shut_down().await;
}

/// The operator's dial overrides for gates: turning a supervised child's dial to
/// `spotter_human` hands its held gate to the operator, who is pushed for it, and its
/// next gate pushes as it opens; its `ask`s and settles still go to its supervisor.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_operators_dial_pushes_gates_but_not_settles() {
    let fixture = Fixture::new().await;
    let kid = fixture.session("kid");
    let sent = capture(&kid);

    let _gate = kid.register_pending(&curl("toolu_1"), Default::default());
    kid.set_review_level(ReviewLevel::SpotterHuman);
    kid.wait_until("the held gate pushed", || sent.lock().unwrap().len() == 1)
        .await;
    assert_eq!(
        sent.lock().unwrap()[0].body,
        "bash — fetch only the response headers"
    );
    assert!(kid.resolve_approval(&"toolu_1".into(), Approval::Approve, Default::default()));

    let _gate = kid.register_pending(&curl("toolu_2"), Default::default());
    assert!(kid.resolve_approval(&"toolu_2".into(), Approval::Approve, Default::default()));
    drop(kid.begin_ask("ask_1", "Which branch?".into(), 1, 1));
    settle(&kid);

    assert_eq!(
        decisions(&kid),
        [
            not_sent(MomentKind::Gate, "boss"),
            (MomentKind::Gate, PushEvent::Sent),
            (MomentKind::Gate, PushEvent::Sent),
            not_sent(MomentKind::Ask, "boss"),
            not_sent(MomentKind::Settle, "boss"),
        ]
    );
    kid.wait_until("both gates sent", || sent.lock().unwrap().len() == 2)
        .await;
    assert!(
        sent.lock()
            .unwrap()
            .iter()
            .all(|message| message.title.starts_with("Gate"))
    );

    fixture.host.swansong().shut_down().await;
}

/// A session with no supervisor pushes its gate, its `ask`, and its settle.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_session_with_no_supervisor_pushes_every_moment() {
    let fixture = Fixture::new().await;
    let solo = fixture.session("solo");
    let sent = capture(&solo);

    let _gate = solo.register_pending(&curl("toolu_1"), Default::default());
    assert!(solo.resolve_approval(&"toolu_1".into(), Approval::Approve, Default::default()));
    drop(solo.begin_ask("ask_1", "Which branch?".into(), 1, 1));
    settle(&solo);

    assert_eq!(
        decisions(&solo),
        [
            (MomentKind::Gate, PushEvent::Sent),
            (MomentKind::Ask, PushEvent::Sent),
            (MomentKind::Settle, PushEvent::Sent),
        ]
    );
    solo.wait_until("three pushes", || sent.lock().unwrap().len() == 3)
        .await;

    fixture.host.swansong().shut_down().await;
}

/// Clearing the supervisor pointer hands a held gate to the operator, who is pushed for
/// it, and the session's next moment pushes as well.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn clearing_the_pointer_pushes_the_held_gate_and_resumes_pushes() {
    let fixture = Fixture::new().await;
    let kid = fixture.session("kid");
    let sent = capture(&kid);

    settle(&kid);
    let _gate = kid.register_pending(&curl("toolu_1"), Default::default());
    fixture.host.set_supervisor("kid", None).unwrap();
    kid.wait_until("the held gate pushed", || sent.lock().unwrap().len() == 1)
        .await;
    assert!(kid.resolve_approval(&"toolu_1".into(), Approval::Approve, Default::default()));
    settle(&kid);

    assert_eq!(
        decisions(&kid),
        [
            not_sent(MomentKind::Settle, "boss"),
            not_sent(MomentKind::Gate, "boss"),
            (MomentKind::Gate, PushEvent::Sent),
            (MomentKind::Settle, PushEvent::Sent),
        ]
    );

    fixture.host.swansong().shut_down().await;
}

/// A supervisor that ends while its child holds a gate leaves the gate with the
/// operator, who is pushed for it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_supervisor_ending_pushes_its_childs_held_gate() {
    let fixture = Fixture::new().await;
    let stray = fixture.session("stray");
    let sent = capture(&stray);

    let _gate = stray.register_pending(&curl("toolu_1"), Default::default());
    fixture.host.end("boss").unwrap().await;
    stray
        .wait_until("the held gate pushed", || sent.lock().unwrap().len() == 1)
        .await;

    assert_eq!(
        decisions(&stray),
        [
            not_sent(MomentKind::Gate, "boss"),
            (MomentKind::Gate, PushEvent::Sent),
        ]
    );

    fixture.host.swansong().shut_down().await;
}

/// A gate escalated to a session that then ends falls back to the operator, who is
/// pushed for it, marked with who escalated it and the note.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_gate_escalated_to_a_session_that_ends_pushes_the_operator() {
    let fixture = Fixture::new().await;
    let leaf = fixture.session("leaf");
    let sent = capture(&leaf);

    let call = curl("toolu_1");
    let _gate = leaf.register_pending(&call, Default::default());
    assert!(leaf.escalate(
        &call.id,
        Some("session[mid]".into()),
        Some("boss".into()),
        Some("out of my depth".into()),
    ));
    fixture.host.end("boss").unwrap().await;
    leaf.wait_until("the held gate pushed", || sent.lock().unwrap().len() == 1)
        .await;

    assert_eq!(
        sent.lock().unwrap()[0].body,
        "escalated by mid (\"out of my depth\"): bash — fetch only the response headers"
    );
    assert_eq!(
        decisions(&leaf),
        [
            not_sent(MomentKind::Gate, "mid"),
            not_sent(MomentKind::Gate, "boss"),
            (MomentKind::Gate, PushEvent::Sent),
        ]
    );

    fixture.host.swansong().shut_down().await;
}
