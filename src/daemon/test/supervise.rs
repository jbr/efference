//! A session's supervisor set after creation: one action carrying gates, driving
//! authority, and a wake subscription (docs/permissions.md, docs/tools.md).

use super::*;
use crate::control_tools::{Approve, Control, Executor, Interrupt, Turn};
use crate::daemon::host::SupervisorRefused;
use crate::daemon::{Host, SessionRuntime};
use crate::frontend::Approval;
use crate::log::{InterruptOrigin, Telemetry};
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
    /// One project: `a`, `b`, and `c` are human roots, and `kid` was commissioned
    /// by `a`. `stranger` is a root in another project.
    fn new() -> Self {
        use std::hash::{BuildHasher, Hasher, RandomState};
        let nonce = RandomState::new().build_hasher().finish();
        let root = std::env::temp_dir().join(format!("efference-supervise-{nonce:016x}"));
        let root = root.canonicalize().unwrap_or(root);
        let projects = [root.join("one"), root.join("two")];
        for project in &projects {
            for id in ["a", "b", "c", "kid", "stranger"] {
                std::fs::create_dir_all(project.join(".efference").join(id)).unwrap();
            }
        }
        let host = Host::new(crate::inference::InferenceRegistry::empty(), baseline_env());
        let adopt = |id: &str, cwd: &Path, created_by: Option<&str>| {
            let boot = runtime::boot_in(cwd, &cwd.join(".efference").join(id));
            host.adopt_with(
                id.to_string(),
                SessionRuntime::start(boot, host.swansong()),
                created_by.map(str::to_string),
                false,
            )
            .expect("the id is free in this test");
        };
        adopt("a", &projects[0], None);
        adopt("b", &projects[0], None);
        adopt("c", &projects[0], None);
        adopt("kid", &projects[0], Some("a"));
        adopt("stranger", &projects[1], None);
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

/// Whether `session` has heard a notification from `source` whose body matches —
/// pending, or already logged as an `Ambient` by its live loop's first show.
fn heard(session: &Session, source: &str, matches: impl Fn(&str) -> bool) -> bool {
    session
        .pending_notifications()
        .iter()
        .any(|n| n.source == source && matches(&n.body))
        || session.snapshot().iter().any(|e| match &e.payload {
            Payload::Ambient(n) => n.source == source && matches(&n.body),
            _ => false,
        })
}

/// The `SupervisorSet` records in `session`'s log, in order.
fn supervisor_records(session: &Session) -> Vec<Option<String>> {
    session
        .snapshot()
        .iter()
        .filter_map(|e| match &e.payload {
            Payload::Telemetry(Telemetry::SupervisorSet { supervisor }) => Some(supervisor.clone()),
            _ => None,
        })
        .collect()
}

/// The `Interrupt` records in `session`'s log, in order.
fn interrupt_origins(session: &Session) -> Vec<InterruptOrigin> {
    session
        .snapshot()
        .iter()
        .filter_map(|e| match &e.payload {
            Payload::Telemetry(Telemetry::Interrupt(origin)) => Some(origin.clone()),
            _ => None,
        })
        .collect()
}

fn leaf() -> ToolCall {
    ToolCall::new(
        "fs__list",
        "toolu_leaf",
        serde_json::json!({ "path": "/Users/x/code", "intent": "locating siblings" }),
    )
}

fn approve(session: &str) -> Approve {
    Approve {
        call_id: Some("toolu_leaf".into()),
        abort: false,
        redirect: None,
        wait: false,
        timeout_s: None,
        session: Some(session.into()),
    }
}

fn turn(session: &str) -> Turn {
    Turn {
        text: vec!["steer".into()],
        wait: false,
        timeout_s: None,
        source: None,
        session: Some(session.into()),
    }
}

/// Setting `b` as the supervisor of `a`, a session the operator created: `b` is told,
/// `a`'s dial moves off the supervised default, its next gate reaches `b` and `b`
/// resolves it, and `a`'s idle after that wakes `b` like a creator's subscription.
/// The change is recorded in `a`'s log.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_set_supervisor_receives_the_gate_approves_it_and_hears_the_idle() {
    let fixture = Fixture::new();
    let a = fixture.session("a");
    let b = fixture.session("b");
    assert_eq!(a.supervisor(), None);
    assert_eq!(a.resolved_review_level(), ReviewLevel::SpotterHuman);

    assert_eq!(
        fixture.host.set_supervisor("a", Some("b".into())),
        Ok(Some("b".into()))
    );
    assert_eq!(a.supervisor().as_deref(), Some("b"));
    assert_eq!(a.review_level(), ReviewLevel::Supervisor);
    let state: Value = serde_json::from_str(&a.state_json()).unwrap();
    assert_eq!(state["supervisor"], "b");
    assert_eq!(state["resolved_review_level"], "supervisor");
    assert_eq!(supervisor_records(&a), [Some("b".to_string())]);
    let subscribers = fixture.host.subscribers("a");
    assert_eq!(subscribers.len(), 1);
    assert_eq!(subscribers[0].0, "b");
    assert!(
        subscribers[0].1.wake,
        "subscribed as a creator is, with wake"
    );
    wait_until("b was told it supervises a", || {
        heard(&b, "a", |body| body.starts_with("You now supervise a"))
    })
    .await;

    let gate = a.register_pending(&leaf(), None);
    wait_until("the gate reached b", || {
        heard(&b, "a", |body| {
            body.contains("You are its supervisor") && body.contains("toolu_leaf")
        })
    })
    .await;
    approve("a")
        .execute(fixture.control("b"), RequestContext::default())
        .await
        .expect("the supervisor may resolve the gate");
    assert!(matches!(gate.await, Ok(Approval::Approve)));
    // The loop runs the approved call and its trajectory ends.
    a.going_idle();

    wait_until("a's idle reached b", || {
        heard(&b, "a", |body| {
            body.starts_with("a finished its turn and is idle")
        })
    })
    .await;

    fixture.host.swansong().shut_down().await;
}

/// The supervisor drives the session it supervises — `turn` and `interrupt` — and a
/// peer that neither created nor supervises it is still refused. Supervision is not
/// transitive: `b` supervising `a` does not reach `kid`, which `a` created.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_supervisor_may_turn_and_interrupt_and_reaches_no_further() {
    let fixture = Fixture::new();
    fixture.host.set_supervisor("a", Some("b".into())).unwrap();
    let b = fixture.control("b");

    turn("a")
        .execute(b.clone(), RequestContext::default())
        .await
        .expect("the supervisor may send a turn");
    let a = fixture.session("a");
    wait_until("the turn landed with b's provenance", || {
        a.snapshot()
            .iter()
            .any(|entry| entry.source.as_deref() == Some("session[b]"))
    })
    .await;
    Interrupt {
        session: Some("a".into()),
    }
    .execute(b.clone(), RequestContext::default())
    .await
    .expect("the supervisor may interrupt");
    assert_eq!(
        interrupt_origins(&fixture.session("a")),
        [InterruptOrigin {
            source: Some("session[b]".into()),
            control: None,
        }],
        "the interrupt tool records the calling session"
    );

    let err = turn("a")
        .execute(fixture.control("c"), RequestContext::default())
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("neither created nor supervises"), "{err}");
    let err = turn("kid")
        .execute(b.clone(), RequestContext::default())
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("neither created nor supervises"), "{err}");

    fixture.host.swansong().shut_down().await;
}

/// A setting that would close the supervisor chain is refused, naming the cycle; so is
/// a session supervising itself, a supervisor that is not hosted, and one in another
/// project. A refused setting changes nothing.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_cycle_and_an_unreachable_supervisor_are_refused() {
    let fixture = Fixture::new();
    fixture.host.set_supervisor("a", Some("b".into())).unwrap();
    fixture.host.set_supervisor("b", Some("c".into())).unwrap();

    let Err(SupervisorRefused::Refused(reason)) =
        fixture.host.set_supervisor("c", Some("a".into()))
    else {
        panic!("a cycle was accepted");
    };
    assert!(reason.contains("c → a → b → c"), "{reason}");
    assert_eq!(fixture.host.supervisor_of("c"), None);

    let Err(SupervisorRefused::Refused(reason)) =
        fixture.host.set_supervisor("c", Some("c".into()))
    else {
        panic!("a self-supervisor was accepted");
    };
    assert!(reason.contains("c → c"), "{reason}");

    for (supervisor, why) in [("ghost", "no session"), ("stranger", "another project")] {
        let Err(SupervisorRefused::Refused(reason)) =
            fixture.host.set_supervisor("c", Some(supervisor.into()))
        else {
            panic!("{supervisor} was accepted");
        };
        assert!(reason.contains(why), "{reason}");
    }
    assert_eq!(
        fixture.host.set_supervisor("ghost", None),
        Err(SupervisorRefused::NotHosted("ghost".into()))
    );
    assert!(supervisor_records(&fixture.session("c")).is_empty());

    fixture.host.swansong().shut_down().await;
}

/// Clearing the pointer while a call is held re-routes that call to the operator, as
/// an ended creator does: the dial stays `supervisor`, the position resolves to
/// `spotter_human`, the gate is still held, and the former supervisor's subscription
/// is gone. Moving the pointer re-routes the held call to the new supervisor.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn clearing_or_moving_the_pointer_reroutes_a_held_call() {
    let fixture = Fixture::new();
    let a = fixture.session("a");
    let c = fixture.session("c");
    fixture.host.set_supervisor("a", Some("b".into())).unwrap();
    let _gate = a.register_pending(&leaf(), None);
    wait_until("the gate reached b", || {
        heard(&fixture.session("b"), "a", |body| {
            body.contains("You are its supervisor")
        })
    })
    .await;

    // Moved: the same held call goes to the new supervisor.
    fixture.host.set_supervisor("a", Some("c".into())).unwrap();
    wait_until("the held call was re-routed to c", || {
        heard(&c, "a", |body| {
            body.contains("You are its supervisor") && body.contains("toolu_leaf")
        })
    })
    .await;
    let subscribers: Vec<String> = fixture
        .host
        .subscribers("a")
        .into_iter()
        .map(|(id, _)| id)
        .collect();
    assert_eq!(
        subscribers,
        ["c"],
        "moving the pointer drops b's subscription"
    );

    // Cleared: the operator answers.
    assert_eq!(fixture.host.set_supervisor("a", None), Ok(None));
    assert_eq!(a.review_level(), ReviewLevel::Supervisor);
    assert_eq!(a.resolved_review_level(), ReviewLevel::SpotterHuman);
    let state: Value = serde_json::from_str(&a.state_json()).unwrap();
    assert!(state["supervisor"].is_null());
    assert!(!state["gate"].is_null(), "re-routed, not dropped");
    assert!(fixture.host.subscribers("a").is_empty());
    assert_eq!(
        supervisor_records(&a),
        [Some("b".to_string()), Some("c".to_string()), None]
    );

    fixture.host.swansong().shut_down().await;
}

/// A set supervisor ending takes the path an ended creator takes: the session it
/// supervised reviews as `spotter_human` and keeps holding its call.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_set_supervisor_ending_hands_the_gate_to_the_operator() {
    let fixture = Fixture::new();
    let a = fixture.session("a");
    fixture.host.set_supervisor("a", Some("b".into())).unwrap();
    let _gate = a.register_pending(&leaf(), None);

    fixture.host.end("b").unwrap().await;
    wait_until("b left the host", || fixture.host.session("b").is_none()).await;
    assert_eq!(a.supervisor(), None);
    assert_eq!(a.resolved_review_level(), ReviewLevel::SpotterHuman);
    assert!(a.pending_gate().is_some());

    fixture.host.swansong().shut_down().await;
}

/// A child with no setting behaves as before: its supervisor is its creator, which
/// may drive it, and nothing is recorded.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_child_with_no_setting_answers_to_its_creator() {
    let fixture = Fixture::new();
    let kid = fixture.session("kid");
    assert_eq!(fixture.host.supervisor_of("kid").as_deref(), Some("a"));
    assert_eq!(kid.supervisor().as_deref(), Some("a"));
    assert_eq!(kid.resolved_review_level(), ReviewLevel::Supervisor);
    assert!(supervisor_records(&kid).is_empty());

    let gate = kid.register_pending(&leaf(), None);
    wait_until("the gate reached the creator", || {
        heard(&fixture.session("a"), "kid", |body| {
            body.contains("You are its supervisor")
        })
    })
    .await;
    approve("kid")
        .execute(fixture.control("a"), RequestContext::default())
        .await
        .expect("the creator may resolve the gate");
    assert!(matches!(gate.await, Ok(Approval::Approve)));

    fixture.host.swansong().shut_down().await;
}
