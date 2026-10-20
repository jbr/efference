//! Who may decide a held gate, apart from whom it is routed to: the session's supervisor
//! and the operator decide, and the review dial picks who is asked (docs/permissions.md,
//! docs/tools.md).

use super::*;
use crate::control_tools::{Control, Decide, Decision, Executor, Message, Status};
use crate::daemon::Host;
use crate::daemon::status::{Condition, Reviewer};
use crate::frontend::Approval;
use crate::log::Telemetry;
use crate::policy::ReviewLevel;
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
    /// One project: `p` and `q` are human roots, and `p` created `c`.
    async fn new() -> Self {
        use std::hash::{BuildHasher, Hasher, RandomState};
        let nonce = RandomState::new().build_hasher().finish();
        let root = std::env::temp_dir().join(format!("efference-decide-authority-{nonce:016x}"));
        let root = root.canonicalize().unwrap_or(root);
        let project = root.join("one");
        for id in ["p", "q", "c"] {
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
        adopt("p", None).await;
        adopt("q", None).await;
        adopt("c", Some("p")).await;
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

/// The must-wake flag of the notification from `c` whose body matches, pending or
/// already logged as an `Ambient` by the live loop's first show; `None` when `session`
/// has heard none.
fn heard_from_c(session: &Session, matches: impl Fn(&str) -> bool) -> Option<bool> {
    if let Some(n) = session
        .pending_notifications()
        .iter()
        .find(|n| n.source == "c" && matches(&n.body))
    {
        return Some(n.wake);
    }
    session.snapshot().iter().find_map(|e| match &e.payload {
        Payload::Ambient(n) if n.source == "c" && matches(&n.body) => Some(n.wake),
        _ => None,
    })
}

fn leaf() -> ToolCall {
    ToolCall::new(
        crate::bash::NAME,
        "toolu_leaf",
        serde_json::json!({
            "command": "curl -sI https://example.com",
            "intent": "fetch only the response headers",
        }),
    )
}

/// Hold the leaf at `c`'s gate, logged first so it has a handle; returns the handle and
/// the gate's receiver.
fn hold(c: &Session) -> (String, tokio::sync::oneshot::Receiver<Approval>) {
    let seq = c
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
        .unwrap()
        .to_string();
    (seq, c.register_pending(&leaf(), None, None))
}

fn decide(seq: &str, decision: Decision) -> Decide {
    Decide {
        agent: "c".into(),
        seq: seq.into(),
        decision,
        message: None,
    }
}

async fn status_of_c(fixture: &Fixture, caller: &str) -> String {
    Status {
        agent: Some("c".into()),
    }
    .execute(fixture.control(caller), RequestContext::default())
    .await
    .unwrap()
}

/// `c`'s gate decisions, in order.
fn decisions(c: &Session) -> Vec<crate::log::GateDecision> {
    c.snapshot()
        .into_iter()
        .filter_map(|e| match e.payload {
            Payload::Telemetry(Telemetry::GateDecision(decision)) => Some(decision),
            _ => None,
        })
        .collect()
}

/// Case A: the operator turns `c`'s dial to `spotter_human` while its creator `p` is
/// still its supervisor. The gate is routed to the operator, and reaches `p`'s inbox
/// without waking it; `p`'s reading of `c` says the operator is asked and names the
/// `decide` call it may make; and `p`'s decision lands.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_supervisor_may_decide_a_gate_routed_to_the_operator_and_is_not_woken() {
    let fixture = Fixture::new().await;
    let p = fixture.session("p");
    let c = fixture.session("c");
    c.set_review_level(ReviewLevel::SpotterHuman);
    assert_eq!(fixture.host.supervisor_of("c").as_deref(), Some("p"));

    let (seq, gate) = hold(&c);
    let Condition::Gated(held) = c.status_view("c", &c.status()).condition else {
        panic!("c holds the call");
    };
    assert_eq!(held.reviewer, Reviewer::Operator, "the operator is asked");

    let first = format!(
        "waiting for a decision from the operator or you on bash `curl -sI \
         https://example.com` (seq {seq})"
    );
    let wake = {
        let mut wake = None;
        wait_until("the gate reached p's inbox", || {
            wake = heard_from_c(&p, |body| body.starts_with(&first));
            wake.is_some()
        })
        .await;
        wake
    };
    assert_eq!(
        wake,
        Some(false),
        "a gate the operator is asked does not wake p"
    );

    let status = status_of_c(&fixture, "p").await;
    assert!(
        status.starts_with(&format!(
            "c: {first}\nintent: fetch only the response headers\ndecide: the operator is asked; \
             as c's supervisor you may decide it too: agents__decide(agent: \"c\", seq: \
             \"{seq}\", decision: \"allow\" | \"deny\", message?)"
        )),
        "{status}"
    );

    decide(&seq, Decision::Allow)
        .execute(fixture.control("p"), RequestContext::default())
        .await
        .expect("the supervisor decides a gate routed to the operator");
    assert!(matches!(gate.await, Ok(Approval::Approve)));
    assert_eq!(
        decisions(&c),
        [crate::log::GateDecision::new(
            "toolu_leaf",
            Approval::Approve,
            Some("session[p]".into())
        )]
    );

    fixture.host.swansong().shut_down().await;
}

/// Case B: the operator moves `c`'s supervisor pointer from `p` to `q`. `p`, which
/// created `c` and still drives it, is refused `decide` — as the verb runs and as the
/// loop binds an allow — naming `q`. `q`'s decision lands, and `p`'s message still
/// reaches `c` as a turn.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_moved_pointer_moves_decide_to_the_new_supervisor() {
    let fixture = Fixture::new().await;
    let c = fixture.session("c");
    fixture.host.set_supervisor("c", Some("q".into())).unwrap();
    let (seq, gate) = hold(&c);

    let refusal = "refused: you don't supervise \"c\", so its gates are not yours to decide — \
        its supervisor, \"q\", and the operator decide them; agents__message and \
        agents__interrupt still reach it";
    for decision in [Decision::Allow, Decision::Deny] {
        let err = decide(&seq, decision)
            .execute(fixture.control("p"), RequestContext::default())
            .await
            .unwrap_err()
            .to_string();
        assert_eq!(err, refusal);
    }
    let allow = ToolCall::new(
        crate::control_tools::DECIDE_WIRE_NAME,
        "toolu_p",
        serde_json::json!({ "agent": "c", "seq": seq, "decision": "allow" }),
    );
    let err = fixture
        .control("p")
        .resolve_bound_action(&allow)
        .unwrap_err()
        .to_string();
    assert_eq!(err, refusal);
    assert!(c.pending_gate().is_some());
    assert!(decisions(&c).is_empty(), "nothing was decided");

    // `p` reads who decides and no move of its own; `q` reads the gate as its own.
    let status = status_of_c(&fixture, "p").await;
    assert!(
        status.starts_with("c: waiting for a decision from q on bash"),
        "{status}"
    );
    assert!(!status.contains("decide:"), "{status}");
    let status = status_of_c(&fixture, "q").await;
    assert!(
        status.starts_with("c: waiting for your decision on bash"),
        "{status}"
    );

    decide(&seq, Decision::Allow)
        .execute(fixture.control("q"), RequestContext::default())
        .await
        .expect("the new supervisor decides");
    assert!(matches!(gate.await, Ok(Approval::Approve)));

    // Driving is unchanged: `p`'s message is `c`'s next turn, not a note in its inbox.
    let said = Message {
        agent: "c".into(),
        text: "after this, stop".into(),
        words: Vec::new(),
        reply_to: None,
    }
    .execute(fixture.control("p"), RequestContext::default())
    .await
    .unwrap();
    assert!(
        said.starts_with("delivered: c") || said.starts_with("queued: c"),
        "{said}"
    );

    fixture.host.swansong().shut_down().await;
}
