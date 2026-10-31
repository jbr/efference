//! `decide` with `escalate`: a held call passed up the supervisor chain undecided, routed
//! by its escalation rather than the dial, and decidable by the session it reached
//! (docs/permissions.md, docs/tools.md).

use super::*;
use crate::control_tools::{Control, Decide, Executor, Status};
use crate::daemon::Host;
use crate::log::{Approval, Telemetry};
use crate::policy::ReviewLevel;
use crate::protocol::agents::Decision;
use crate::protocol::status::{Condition, Escalated, Reviewer};
use mcplease::traits::Tool as _;
use mcplease::types::RequestContext;

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
    /// One project: `g` created `p`, which created `c` — a two-level chain — and `q`
    /// created `k`, a one-level chain whose top is the operator.
    async fn new() -> Self {
        use std::hash::{BuildHasher, Hasher, RandomState};
        let nonce = RandomState::new().build_hasher().finish();
        let root = std::env::temp_dir().join(format!("efference-escalate-{nonce:016x}"));
        let root = root.canonicalize().unwrap_or(root);
        let project = root.join("one");
        for id in ["g", "p", "c", "q", "k"] {
            std::fs::create_dir_all(project.join(".efference").join(id)).unwrap();
        }
        let host = Host::new(crate::inference::InferenceRegistry::empty(), baseline_env());
        let adopt = async |id: &str, created_by: Option<&str>| {
            let boot = runtime::boot_in(&project, &project.join(".efference").join(id));
            host.adopt_with(
                id.to_string(),
                runtime::started(boot, host.swansong()).await,
                created_by.map(str::to_string),
                false,
            )
            .expect("the id is free in this test");
        };
        adopt("g", None).await;
        adopt("p", Some("g")).await;
        adopt("c", Some("p")).await;
        adopt("q", None).await;
        adopt("k", Some("q")).await;
        Self { root, host }
    }

    fn control(&self, caller: &str) -> Control {
        Control::new(Executor::host(&self.host, caller.to_string()), false)
    }

    fn session(&self, id: &str) -> Arc<Session> {
        self.host.session(id).expect(id)
    }
}

/// The latest notification body from `source`, pending or logged.
fn latest_from(session: &Session, source: &str) -> Option<String> {
    let logged = session
        .snapshot()
        .iter()
        .rev()
        .find_map(|e| match &e.payload {
            Payload::Ambient(n) if n.source == source => Some(n.body.clone()),
            _ => None,
        });
    session
        .pending_notifications()
        .iter()
        .rev()
        .find(|n| n.source == source)
        .map(|n| n.body.clone())
        .or(logged)
}

fn curl(id: &str) -> ToolCall {
    ToolCall::new(
        crate::bash::NAME,
        id,
        serde_json::json!({
            "command": "curl -sI https://example.com",
            "intent": "fetch only the response headers",
        }),
    )
}

/// Hold `call` at `holder`'s gate, logged first so it has a handle; returns the handle
/// and the gate's receiver.
fn hold(holder: &Session, call: &ToolCall) -> (String, oneshot::Receiver<Approval>) {
    let seq = holder
        .append(
            1,
            Payload::assistant(crate::log::AssistantOutput::from_legacy_messages(
                vec![],
                vec![],
                vec![call.clone()],
            )),
        )
        .unwrap()
        .to_string();
    (seq, holder.register_pending(call, Default::default()))
}

fn decide(agent: &str, seq: &str, decision: Decision, message: Option<&str>) -> Decide {
    Decide {
        agent: agent.into(),
        seq: seq.into(),
        decision,
        message: message.map(str::to_string),
    }
}

fn held(session: &Session, id: &str) -> crate::protocol::status::HeldCall {
    match session.status_view(id, &session.status()).condition {
        Condition::Gated(held) => held,
        other => panic!("{id} holds no call: {other:?}"),
    }
}

/// `session`'s escalation records, in order.
fn escalations(session: &Session) -> Vec<crate::log::Escalation> {
    session
        .snapshot()
        .into_iter()
        .filter_map(|e| match e.payload {
            Payload::Telemetry(Telemetry::Escalation(escalation)) => Some(escalation),
            _ => None,
        })
        .collect()
}

fn gate_decisions(session: &Session) -> Vec<crate::log::GateDecision> {
    session
        .snapshot()
        .into_iter()
        .filter_map(|e| match e.payload {
            Payload::Telemetry(Telemetry::GateDecision(decision)) => Some(decision),
            _ => None,
        })
        .collect()
}

const NOTE: &str = "I can't tell whether example.com is in scope";

/// A two-level chain: `c` gates to `p`; `p` escalates, so the call stays held and goes
/// to `g`, woken as its reviewer with `p`'s note, while `p` reads it without wake as
/// escalated to `g`; the escalation is recorded; `g` decides it, by the verb and by the
/// loop's binding of its allow.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_two_level_chain_escalates_to_the_grandparent() {
    let fixture = Fixture::new().await;
    let (g, p, c) = (
        fixture.session("g"),
        fixture.session("p"),
        fixture.session("c"),
    );
    assert_eq!(c.resolved_review_level(), ReviewLevel::Supervisor);
    let (seq, gate) = hold(&c, &curl("toolu_c"));
    assert_eq!(
        held(&c, "c").reviewer,
        Reviewer::Supervisor { id: "p".into() }
    );
    p.wait_until("the gate reached p as its reviewer", || {
        heard(&p, |n| {
            n.source == "c" && n.body.starts_with("waiting for your decision")
        })
        .is_some_and(|n| n.wake)
    })
    .await;

    let said = decide("c", &seq, Decision::Escalate, Some(NOTE))
        .execute(fixture.control("p"), RequestContext::default())
        .await
        .unwrap();
    assert_eq!(
        said,
        "escalated to g: c still holds bash `curl -sI https://example.com` until g decides \
         it and reads your note"
    );
    let now = held(&c, "c");
    assert_eq!(now.reviewer, Reviewer::Supervisor { id: "g".into() });
    assert_eq!(
        now.escalations,
        [Escalated {
            source: Some("session[p]".into()),
            to: Some("g".into()),
            note: Some(NOTE.into()),
        }]
    );
    assert_eq!(
        escalations(&c),
        [crate::log::Escalation::new(
            "toolu_c",
            Some("session[p]".into()),
            Some("g".into()),
            Some(NOTE.into()),
        )]
    );

    // `g` is woken as the reviewer and reads the note; `p` hears it went to `g`, unwoken.
    let reviewer = format!(
        "waiting for your decision on bash `curl -sI https://example.com` (seq \
         {seq})\nescalated: by p to you: \"{NOTE}\"\nintent: fetch only the response headers"
    );
    g.wait_until("the escalated gate reached g as its reviewer", || {
        heard(&g, |n| n.source == "c" && n.body.starts_with(&reviewer)).is_some_and(|n| n.wake)
    })
    .await;
    p.wait_until("p heard the gate went to g", || {
        heard(&p, |n| {
            n.source == "c"
                && n.body
                    .starts_with("waiting for a decision from g or you on bash")
                && n.body
                    .contains(&format!("\nescalated: by you to g: \"{NOTE}\""))
        })
        .is_some_and(|n| !n.wake)
    })
    .await;
    let status = Status { agent: "c".into() }
        .execute(fixture.control("g"), RequestContext::default())
        .await
        .unwrap();
    assert!(status.starts_with(&format!("c: {reviewer}")), "{status}");
    assert!(
        status.contains("\ndecide: agents__decide(agent: \"c\""),
        "{status}"
    );
    assert!(
        status.contains("\nescalate: agents__decide(agent: \"c\""),
        "{status}"
    );

    // `g` does not supervise `c`, yet the loop binds its allow of this gate.
    let allow = ToolCall::new(
        crate::protocol::agents::DECIDE_WIRE_NAME,
        "toolu_g",
        serde_json::json!({ "agent": "c", "seq": seq, "decision": "allow" }),
    );
    let bound = fixture.control("g").resolve_bound_action(&allow).unwrap();
    assert_eq!(bound.chain, ["c"]);

    decide("c", &seq, Decision::Allow, None)
        .execute(fixture.control("g"), RequestContext::default())
        .await
        .expect("the session the gate was escalated to decides it");
    assert!(matches!(gate.await, Ok(Approval::Approve)));
    assert_eq!(
        gate_decisions(&c),
        [crate::log::GateDecision::new(
            "toolu_c",
            Approval::Approve,
            Some("session[g]".into())
        )]
    );

    fixture.host.swansong().shut_down().await;
}

/// The escalation grants the one gate: `g`'s decide naming any other seq of `c`, or
/// `c`'s next gate after this one is decided, is refused as from a non-supervisor.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_grandparent_decides_only_the_escalated_gate() {
    let fixture = Fixture::new().await;
    let c = fixture.session("c");
    let (seq, gate) = hold(&c, &curl("toolu_1"));
    decide("c", &seq, Decision::Escalate, None)
        .execute(fixture.control("p"), RequestContext::default())
        .await
        .unwrap();

    // `g` drives `c`, its grandchild, without supervising it.
    let refusal = "refused: you don't supervise \"c\", so its gates are not yours to decide — \
        its supervisor, \"p\", and the operator decide them; agents__message and \
        agents__interrupt still reach it";
    let err = decide("c", "999", Decision::Allow, None)
        .execute(fixture.control("g"), RequestContext::default())
        .await
        .unwrap_err()
        .to_string();
    assert_eq!(err, refusal);

    decide("c", &seq, Decision::Deny, Some("not that host"))
        .execute(fixture.control("p"), RequestContext::default())
        .await
        .expect("the supervisor may still decide a gate it escalated");
    assert!(matches!(gate.await, Ok(Approval::Redirect(_))));

    let (next, _gate) = hold(&c, &curl("toolu_2"));
    for decision in [Decision::Allow, Decision::Deny, Decision::Escalate] {
        let err = decide("c", &next, decision, None)
            .execute(fixture.control("g"), RequestContext::default())
            .await
            .unwrap_err()
            .to_string();
        assert_eq!(err, refusal);
    }
    let allow = ToolCall::new(
        crate::protocol::agents::DECIDE_WIRE_NAME,
        "toolu_g",
        serde_json::json!({ "agent": "c", "seq": next, "decision": "allow" }),
    );
    let err = fixture
        .control("g")
        .resolve_bound_action(&allow)
        .unwrap_err()
        .to_string();
    assert_eq!(err, refusal);

    fixture.host.swansong().shut_down().await;
}

/// A one-level chain: `k` gates to `q`, which has no supervisor, so its escalation goes
/// to the operator — the gate topic carries who escalated it and the note, and a push
/// goes out marked "escalated by q" with the note. The gate itself, routed to `q`,
/// pushed nothing (docs/web-push.md).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_one_level_chain_reaches_the_operator_with_a_push() {
    let fixture = Fixture::new().await;
    let (q, k) = (fixture.session("q"), fixture.session("k"));
    let sent = Arc::new(Mutex::new(Vec::<crate::push::Message>::new()));
    {
        let sent = Arc::clone(&sent);
        k.set_push_transport(Arc::new(move |message| {
            sent.lock().unwrap().push(message);
            Box::pin(async { Vec::new() })
        }));
    }
    let (seq, _gate) = hold(&k, &curl("toolu_k"));
    let said = decide("k", &seq, Decision::Escalate, Some(NOTE))
        .execute(fixture.control("q"), RequestContext::default())
        .await
        .unwrap();
    assert_eq!(
        said,
        "escalated to the operator: k still holds bash `curl -sI https://example.com` until \
         the operator decides it and reads your note"
    );
    assert_eq!(held(&k, "k").reviewer, Reviewer::Operator);
    assert_eq!(
        k.topic_json(Topic::Gate)["escalations"],
        serde_json::json!([{ "source": "session[q]", "to": null, "note": NOTE }])
    );
    k.wait_until("the escalation's push went out", || {
        sent.lock().unwrap().len() == 1
    })
    .await;
    let push = sent.lock().unwrap()[0].clone();
    assert_eq!(
        push.body,
        format!("escalated by q (\"{NOTE}\"): bash — fetch only the response headers")
    );
    assert!(push.title.starts_with("Gate"), "{}", push.title);

    // `q` escalated it, so the operator is asked and `q` is not woken by it.
    q.wait_until("q heard the gate went to the operator", || {
        heard(&q, |n| {
            n.source == "k" && n.body.contains("\nescalated: by you to the operator")
        })
        .is_some_and(|n| !n.wake)
    })
    .await;
    // A session it did not reach cannot pass it on again.
    let err = decide("k", &seq, Decision::Escalate, None)
        .execute(fixture.control("q"), RequestContext::default())
        .await
        .unwrap_err()
        .to_string();
    assert_eq!(
        err,
        "refused: k's bash `curl -sI https://example.com` (seq 1) is routed to the operator, \
         not to you — escalate passes on a gate routed to you; allow or deny still decide it"
            .replace("seq 1", &format!("seq {seq}"))
    );

    fixture.host.swansong().shut_down().await;
}

/// The dial does not route an escalated gate back: turning `c`'s dial away from
/// `supervisor` and back leaves it with `g`, and `p` is not handed it again.
#[tokio::test(start_paused = true)]
async fn the_dial_does_not_route_an_escalated_gate_back() {
    let fixture = Fixture::new().await;
    let (g, p, c) = (
        fixture.session("g"),
        fixture.session("p"),
        fixture.session("c"),
    );
    let (seq, _gate) = hold(&c, &curl("toolu_1"));
    decide("c", &seq, Decision::Escalate, None)
        .execute(fixture.control("p"), RequestContext::default())
        .await
        .unwrap();
    g.wait_until("the escalated gate reached g", || {
        heard(&g, |n| {
            n.source == "c" && n.body.starts_with("waiting for your decision")
        })
        .is_some_and(|n| n.wake)
    })
    .await;

    for level in [ReviewLevel::SpotterHuman, ReviewLevel::Supervisor] {
        c.set_review_level(level);
        assert_eq!(
            held(&c, "c").reviewer,
            Reviewer::Supervisor { id: "g".into() },
            "{level:?}"
        );
    }
    tokio::time::sleep(Duration::from_millis(200)).await;
    let latest = latest_from(&p, "c").expect("p heard c");
    assert!(
        latest.starts_with("waiting for a decision from g or you"),
        "{latest}"
    );

    fixture.host.swansong().shut_down().await;
}
