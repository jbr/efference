//! The in-session `agents__*` tools: a session driving another through the host
//! directly, with the caller's identity enforced (docs/tools.md).

use super::*;
use crate::control_tools::{Archive, Control, Create, Executor, Interrupt, Message, Status};
use crate::daemon::{Host, SessionRuntime};
use mcplease::traits::Tool as _;
use mcplease::types::RequestContext;
use std::path::Path;

mod decisions;
mod messages;
mod reads;

pub(super) struct Fixture {
    root: PathBuf,
    host: Arc<Host>,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

impl Fixture {
    /// A host with five sessions over two projects. `parent`, `peer`, `child`, and
    /// `grandchild` share one `.efference/` base (`child` commissioned by `parent`,
    /// `grandchild` by `child`); `stranger` sits in another project.
    fn new() -> Self {
        use std::hash::{BuildHasher, Hasher, RandomState};
        let nonce = RandomState::new().build_hasher().finish();
        let root = std::env::temp_dir().join(format!("efference-in-session-{nonce:016x}"));
        let root = root.canonicalize().unwrap_or(root);
        let projects = [root.join("a"), root.join("b")];
        let ids = ["parent", "peer", "child", "grandchild", "stranger"];
        for project in &projects {
            for id in ids {
                std::fs::create_dir_all(project.join(".efference").join(id)).unwrap();
            }
        }
        let host = Host::new(crate::inference::InferenceRegistry::empty(), baseline_env());
        let adopt = |id: &str, cwd: &Path, created_by: Option<String>| {
            let boot = runtime::boot_in(cwd, &cwd.join(".efference").join(id));
            host.adopt_with(
                id.to_string(),
                SessionRuntime::start(boot, host.swansong()),
                created_by,
                false,
            )
            .expect("the id is free in this test");
        };
        adopt("parent", &projects[0], None);
        adopt("peer", &projects[0], None);
        adopt("child", &projects[0], Some("parent".into()));
        adopt("grandchild", &projects[0], Some("child".into()));
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

/// Log `calls` as one assistant turn of `session`'s, so each has a transcript handle;
/// returns the turn's seq.
fn issue(session: &Session, calls: Vec<ToolCall>) -> Seq {
    session
        .append(
            1,
            Payload::Assistant {
                output: crate::log::AssistantOutput::from_legacy_messages(vec![], vec![], calls),
                upstream_failure: None,
            },
        )
        .unwrap()
}

/// A bash call the child holds for a decision.
fn curl() -> ToolCall {
    ToolCall::new(
        crate::bash::NAME,
        "toolu_curl",
        serde_json::json!({
            "command": "curl -sI https://example.com",
            "intent": "fetch only the response headers",
        }),
    )
}

/// A message from the caller to `agent`.
fn message(agent: &str, text: &str, reply_to: Option<&str>) -> Message {
    Message {
        agent: Some(agent.into()),
        text: text.into(),
        words: Vec::new(),
        reply_to: reply_to.map(str::to_string),
    }
}

async fn wait_for(what: &str, mut done: impl FnMut() -> bool) {
    for _ in 0..250 {
        if done() {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    panic!("timed out waiting for {what}");
}

/// Rule 1: a session is never a target of itself, on any verb.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn self_target_is_refused() {
    let fixture = Fixture::new();
    let control = fixture.control("parent");
    let err = Interrupt {
        agent: Some("parent".into()),
    }
    .execute(control.clone(), RequestContext::default())
    .await
    .unwrap_err()
    .to_string();
    assert!(err.contains("a session cannot address itself"), "{err}");

    // A message naming no agent defaults to the caller, which the same rule refuses
    // rather than silently messaging itself.
    let err = Message {
        agent: None,
        ..message("x", "hello", None)
    }
    .execute(control.clone(), RequestContext::default())
    .await
    .unwrap_err()
    .to_string();
    assert!(err.contains("a session cannot address itself"), "{err}");

    fixture.host.swansong().shut_down().await;
}

/// Project scope is a visibility boundary: a session sharing no `.efference/`
/// base with the caller is not found, not forbidden, and the overview leaves it out.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_session_in_another_project_is_not_found() {
    let fixture = Fixture::new();
    let control = fixture.control("parent");
    let err = Status {
        agent: Some("stranger".into()),
    }
    .execute(control.clone(), RequestContext::default())
    .await
    .unwrap_err()
    .to_string();
    assert!(
        err.contains("no agent \"stranger\" in this project"),
        "{err}"
    );
    assert!(!err.contains("drive"), "not a permission refusal: {err}");

    let overview = Status { agent: None }
        .execute(control.clone(), RequestContext::default())
        .await
        .unwrap();
    assert!(!overview.contains("stranger"), "{overview}");

    fixture.host.swansong().shut_down().await;
}

/// A parent's allowing `agents__decide` binds to the target's held move
/// (docs/permissions.md): the leaf tool, its input as the child wrote it, its intent,
/// its cwd, and the chain.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_bound_decide_resolves_the_childs_held_move() {
    let fixture = Fixture::new();
    let child = fixture.session("child");
    let leaf = ToolCall::new(
        "bash",
        "toolu_leaf",
        serde_json::json!({ "command": "rm -rf build", "intent": "clean the tree" }),
    );
    let seq = issue(&child, vec![leaf.clone()]);
    let _gate = child.register_pending(&leaf, None);

    let decide = ToolCall::new(
        crate::control_tools::DECIDE_WIRE_NAME,
        "toolu_parent",
        serde_json::json!({ "agent": "child", "seq": seq.to_string(), "decision": "allow" }),
    );
    let bound = fixture
        .control("parent")
        .resolve_bound_action(&decide)
        .expect("a driven session with a matching pending gate binds");
    assert_eq!(bound.name, "bash");
    assert_eq!(bound.input["command"], "rm -rf build");
    assert_eq!(bound.intent.as_deref(), Some("clean the tree"));
    assert_eq!(bound.cwd, fixture.root.join("a"));
    assert_eq!(bound.session, "child");
    assert_eq!(bound.chain, vec!["child".to_string()]);

    fixture.host.swansong().shut_down().await;
}

/// An allow of an allow is never the thing authorized (docs/permissions.md): the
/// resolver follows the held decision through the host to the leaf, accumulating the
/// chain of participating sessions.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_decide_of_a_decide_carries_a_two_hop_chain() {
    let fixture = Fixture::new();
    let grandchild = fixture.session("grandchild");
    let leaf = ToolCall::new(
        "bash",
        "toolu_leaf",
        serde_json::json!({ "command": "cargo test" }),
    );
    let leaf_seq = issue(&grandchild, vec![leaf.clone()]);
    let _leaf_gate = grandchild.register_pending(&leaf, None);

    let inner = ToolCall::new(
        crate::control_tools::DECIDE_WIRE_NAME,
        "toolu_inner",
        serde_json::json!({
            "agent": "grandchild", "seq": leaf_seq.to_string(), "decision": "allow"
        }),
    );
    let child = fixture.session("child");
    let inner_seq = issue(&child, vec![inner.clone()]);
    let _inner_gate = child.register_pending(&inner, None);

    let outer = ToolCall::new(
        crate::control_tools::DECIDE_WIRE_NAME,
        "toolu_outer",
        serde_json::json!({ "agent": "child", "seq": inner_seq, "decision": "allow" }),
    );
    let bound = fixture
        .control("parent")
        .resolve_bound_action(&outer)
        .expect("the chain follows to the leaf");
    assert_eq!(bound.name, "bash");
    assert_eq!(bound.input["command"], "cargo test");
    assert_eq!(bound.session, "child");
    assert_eq!(
        bound.chain,
        vec!["child".to_string(), "grandchild".to_string()]
    );

    fixture.host.swansong().shut_down().await;
}

/// A seq that is not the target's pending gate refuses with "gate moved" before any
/// classification (docs/permissions.md), a target with no gate at all refuses the same
/// way, and a decision naming no seq is refused for want of one.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_stale_absent_or_unnamed_gate_is_refused() {
    let fixture = Fixture::new();
    let child = fixture.session("child");
    let real = ToolCall::new("bash", "toolu_real", serde_json::json!({ "command": "ls" }));
    let seq = issue(&child, vec![real.clone()]);
    let _gate = child.register_pending(&real, None);
    let decide = |agent: &str, seq: Option<String>| {
        let mut input = serde_json::json!({ "agent": agent, "decision": "allow" });
        if let Some(seq) = seq {
            input["seq"] = seq.into();
        }
        ToolCall::new(
            crate::control_tools::DECIDE_WIRE_NAME,
            "toolu_parent",
            input,
        )
    };
    let refused = |call: ToolCall| {
        fixture
            .control("parent")
            .resolve_bound_action(&call)
            .unwrap_err()
            .to_string()
    };

    let err = refused(decide("child", Some(format!("{}", seq + 7))));
    assert!(err.contains("gate moved"), "{err}");
    assert!(err.contains(&format!("it holds {seq}")), "{err}");
    // A driven session with no gate open at all (the tree is parent → child →
    // grandchild).
    let err = refused(decide("grandchild", Some("3".into())));
    assert!(err.contains("gate moved"), "{err}");
    let err = refused(decide("child", None));
    assert!(err.contains("takes `seq`"), "{err}");

    fixture.host.swansong().shut_down().await;
}

/// The target rules the verb enforces apply to the binding too (docs/permissions.md):
/// self is never a target, and a driving verb reaches driven sessions only.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_target_rules_apply_to_a_bound_decide() {
    let fixture = Fixture::new();
    let control = fixture.control("parent");
    let decide = |agent: &str| {
        ToolCall::new(
            crate::control_tools::DECIDE_WIRE_NAME,
            "toolu_parent",
            serde_json::json!({ "agent": agent, "seq": "1", "decision": "allow" }),
        )
    };
    let err = control
        .resolve_bound_action(&decide("parent"))
        .unwrap_err()
        .to_string();
    assert!(err.contains("a session cannot address itself"), "{err}");
    let err = control
        .resolve_bound_action(&decide("peer"))
        .unwrap_err()
        .to_string();
    assert!(err.contains("you don't drive \"peer\""), "{err}");

    fixture.host.swansong().shut_down().await;
}

/// A session creates only inside its own project (docs/tools.md): a cwd whose
/// `.efference/` base is not the caller's is refused before anything is minted,
/// since no later verb could reach the child. A cwd inside the project passes
/// this check, and fails later here on the empty inference registry, with an error
/// that says what the host offers.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_create_outside_the_project_is_refused() {
    let fixture = Fixture::new();
    let control = fixture.control("parent");
    let create = |cwd: &str| Create {
        cwd: Some(cwd.into()),
        name: Some("offshoot".into()),
        message: None,
        model: Some("frontier/supervisor-9".into()),
        spotter_model: None,
        config: None,
        effort: None,
        temperature: None,
        autonomous: false,
        upstream: Vec::new(),
        detached: false,
        worktree: false,
    };
    let err = create("../b")
        .execute(control.clone(), RequestContext::default())
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("outside this project"), "{err}");
    assert!(
        err.contains(&fixture.root.join("b").display().to_string()),
        "{err}"
    );
    assert!(
        !fixture.root.join("b/.efference/offshoot").exists(),
        "nothing minted"
    );

    let err = create("./sub/..")
        .execute(control.clone(), RequestContext::default())
        .await
        .unwrap_err()
        .to_string();
    assert!(!err.contains("outside this project"), "{err}");
    assert!(
        err.contains("this host has no connections configured"),
        "a model the host cannot run names what it can: {err}"
    );

    // A cwd inside one of the project's worktrees belongs to the project, as it does
    // for a caller working in one (docs/worktrees.md).
    let err = create("./.efference/worktrees/w/src")
        .execute(control.clone(), RequestContext::default())
        .await
        .unwrap_err()
        .to_string();
    assert!(!err.contains("outside this project"), "{err}");

    fixture.host.swansong().shut_down().await;
}

/// A succession (docs/tools.md): the successor's first turn is the brief under the
/// reserved hand-off source, the predecessor names its successor for the clients to
/// follow, and the successor — which drives its predecessor — may archive it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_successor_gets_the_brief_and_may_archive_its_predecessor() {
    let fixture = Fixture::new();
    fixture
        .host
        .record_succession("parent", "peer", "carry on".into());

    let parent = fixture.session("parent");
    assert_eq!(
        parent.identity().and_then(|identity| identity.successor),
        Some("peer".to_string())
    );
    let peer = fixture.session("peer");
    wait_for("the brief", || {
        peer.snapshot().iter().any(|entry| {
            entry.source.as_deref() == Some("handoff[parent]")
                && matches!(&entry.payload, crate::log::Payload::User(_))
        })
    })
    .await;

    // A session that is neither a driver nor the successor is refused.
    let err = Archive {
        agent: "parent".into(),
    }
    .execute(fixture.control("stranger"), RequestContext::default())
    .await
    .unwrap_err()
    .to_string();
    assert!(
        err.contains("no agent") || err.contains("don't drive"),
        "{err}"
    );

    // `child` was the predecessor's, so it ends with it.
    let said = Archive {
        agent: "parent".into(),
    }
    .execute(fixture.control("peer"), RequestContext::default())
    .await
    .expect("the successor archives its predecessor");
    assert_eq!(
        said,
        "archived parent and the 2 sessions it created (child, grandchild); logs stay on disk"
    );
    wait_for("the predecessor to leave", || {
        fixture.host.session("parent").is_none()
    })
    .await;

    fixture.host.swansong().shut_down().await;
}

/// The hand-off's completion clears on the fast path (docs/permissions.md): only an
/// `archive` of the caller's own predecessor, by the host's record, counts. The same call
/// naming a sibling or a descendant, or from a session that is not the successor, does not.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn only_a_successors_archive_of_its_predecessor_completes_the_hand_off() {
    let fixture = Fixture::new();
    fixture
        .host
        .record_succession("parent", "peer", "carry on".into());
    let toolbox_for = |caller: &str| {
        let mut toolbox = crate::tools::Toolbox::bash_only();
        toolbox.mount_sessions(Arc::clone(&fixture.host), caller.to_string());
        toolbox
    };
    let archive = |target: &str| {
        crate::log::ToolCall::new(
            crate::control_tools::ARCHIVE_WIRE_NAME,
            "toolu_1",
            serde_json::json!({ "agent": target }),
        )
    };

    assert!(toolbox_for("peer").completes_hand_off(&archive("parent")));
    // The successor's other targets, and another session's archive of the predecessor.
    assert!(!toolbox_for("peer").completes_hand_off(&archive("stranger")));
    assert!(!toolbox_for("peer").completes_hand_off(&archive("child")));
    assert!(!toolbox_for("child").completes_hand_off(&archive("parent")));
    assert!(!toolbox_for("stranger").completes_hand_off(&archive("parent")));
    // The predecessor is not the successor of its successor.
    assert!(!toolbox_for("parent").completes_hand_off(&archive("peer")));
    // Another tool naming the predecessor is not the completion.
    let note = crate::log::ToolCall::new(
        "agents__message",
        "toolu_2",
        serde_json::json!({ "agent": "parent", "text": "done?" }),
    );
    assert!(!toolbox_for("peer").completes_hand_off(&note));
    // No sessions provider mounted: nothing clears.
    assert!(!crate::tools::Toolbox::bash_only().completes_hand_off(&archive("parent")));

    fixture.host.swansong().shut_down().await;
}

/// A successor is created with its predecessor's inference options, not the
/// host's defaults (docs/tools.md): a supervisor on a large model must not hand
/// its place to a smaller one. The empty registry refuses the model, and the
/// refusal names the one the predecessor was created with.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_successor_is_created_with_its_predecessors_options() {
    let fixture = Fixture::new();
    fixture.host.record_options(
        "parent",
        crate::inference::SessionOptions {
            model: Some("frontier/supervisor-9".into()),
            effort: Some("max".into()),
            ..Default::default()
        },
    );
    let body = CreateSession {
        env: Default::default(),
        ..CreateSession::new(fixture.root.join("a"))
    };
    let err = fixture
        .host
        .hand_off("parent", body, "carry on".into())
        .await
        .expect_err("the empty registry refuses the carried model");
    assert!(err.contains("frontier"), "{err}");

    fixture.host.swansong().shut_down().await;
}
