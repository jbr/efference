//! The in-session `sessions` tools: a session driving another through the host
//! directly, with the caller's identity enforced (docs/tools.md).

use super::*;
use crate::control_tools::{
    Archive, Control, Create, Executor, Interrupt, Notify, Sessions, State, Turn,
};
use crate::daemon::{Host, SessionRuntime};
use mcplease::traits::Tool as _;
use mcplease::traits::ToolOutput as _;
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
    /// A host with four sessions over two projects. `parent`, `peer`, and `child`
    /// share one `.efference/` base (`child` commissioned by `parent`);
    /// `stranger` sits in another project.
    fn new() -> Self {
        use std::hash::{BuildHasher, Hasher, RandomState};
        let nonce = RandomState::new().build_hasher().finish();
        let root = std::env::temp_dir().join(format!("efference-in-session-{nonce:016x}"));
        let root = root.canonicalize().unwrap_or(root);
        let projects = [root.join("a"), root.join("b")];
        let ids = ["parent", "peer", "child", "stranger"];
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
}

/// Rule 1: a session is never a target of itself, on any verb.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn self_target_is_refused() {
    let fixture = Fixture::new();
    let control = fixture.control("parent");
    let err = Interrupt {
        session: Some("parent".into()),
    }
    .execute(control.clone(), RequestContext::default())
    .await
    .unwrap_err()
    .to_string();
    assert!(err.contains("cannot target itself"), "{err}");

    // A call naming no session defaults to the caller, which the same rule
    // refuses rather than silently reading the caller's own state.
    // `StateResult` has no `Debug`, so take the error rather than `unwrap_err`.
    let err = State { session: None }
        .execute(control.clone(), RequestContext::default())
        .await
        .err()
        .expect("a self-target is refused")
        .to_string();
    assert!(err.contains("cannot target itself"), "{err}");

    fixture.host.swansong().shut_down().await;
}

/// Rule 2: a mutating verb reaches descendants only. A peer in the same project
/// is refused a `turn`, while `notify` — which is not a mutation — reaches it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_non_descendant_mutation_is_refused_but_a_notify_reaches_it() {
    let fixture = Fixture::new();
    let control = fixture.control("parent");
    let err = Turn {
        text: vec!["steer".into()],
        wait: false,
        timeout_s: None,
        source: None,
        session: Some("peer".into()),
    }
    .execute(control.clone(), RequestContext::default())
    .await
    .unwrap_err()
    .to_string();
    assert!(err.contains("neither created nor supervises"), "{err}");

    Notify {
        source: "parent".into(),
        body: vec!["hello peer".into()],
        slot: None,
        session: Some("peer".into()),
    }
    .execute(control.clone(), RequestContext::default())
    .await
    .expect("notify reaches anything in the project");

    fixture.host.swansong().shut_down().await;
}

/// A descendant `turn` lands on the child's log carrying the caller's
/// `session[<id>]` provenance (docs/tools.md).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_descendant_turn_lands_with_the_session_source() {
    let fixture = Fixture::new();
    let control = fixture.control("parent");
    Turn {
        text: vec!["do the thing".into()],
        wait: false,
        timeout_s: None,
        source: None,
        session: Some("child".into()),
    }
    .execute(control.clone(), RequestContext::default())
    .await
    .expect("a descendant is a valid target");

    let child = fixture.host.session("child").unwrap();
    let landed = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if child
                .snapshot()
                .iter()
                .any(|entry| entry.source.as_deref() == Some("session[parent]"))
            {
                return;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await;
    assert!(landed.is_ok(), "the child never logged the submitted turn");

    fixture.host.swansong().shut_down().await;
}

/// Project scope is a visibility boundary: a session sharing no `.efference/`
/// base with the caller is not found, not forbidden, and does not appear in the
/// caller's `sessions` listing.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_session_in_another_project_is_not_found() {
    let fixture = Fixture::new();
    let control = fixture.control("parent");
    let err = State {
        session: Some("stranger".into()),
    }
    .execute(control.clone(), RequestContext::default())
    .await
    .err()
    .expect("a session in another project is not found")
    .to_string();
    assert!(err.contains("no session"), "{err}");
    assert!(err.contains("in this project"), "{err}");
    assert!(!err.contains("ancestor"), "not a permission refusal: {err}");

    let listing = Sessions {}
        .execute(control.clone(), RequestContext::default())
        .await
        .expect("the project lists")
        .to_text();
    for id in ["parent", "peer", "child", "\"created_by\": \"parent\""] {
        assert!(listing.contains(id), "{id} missing from {listing}");
    }
    assert!(!listing.contains("stranger"), "{listing}");

    fixture.host.swansong().shut_down().await;
}

/// A parent's `sessions__approve` binds to the target's held move (docs/permissions.md):
/// the leaf tool, its input as the child wrote it, its intent, its cwd, and the chain.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_bound_approve_resolves_the_childs_held_move() {
    let fixture = Fixture::new();
    let child = fixture.host.session("child").unwrap();
    let leaf = ToolCall::new(
        "bash",
        "toolu_leaf",
        serde_json::json!({ "command": "rm -rf build", "intent": "clean the tree" }),
    );
    let _gate = child.register_pending(&leaf, None);

    let approve = ToolCall::new(
        crate::control_tools::APPROVE_WIRE_NAME,
        "toolu_parent",
        serde_json::json!({ "session": "child", "call_id": "toolu_leaf" }),
    );
    let bound = fixture
        .control("parent")
        .resolve_bound_action(&approve)
        .expect("a descendant with a matching pending gate binds");
    assert_eq!(bound.name, "bash");
    assert_eq!(bound.input["command"], "rm -rf build");
    assert_eq!(bound.intent.as_deref(), Some("clean the tree"));
    assert_eq!(bound.cwd, fixture.root.join("a"));
    assert_eq!(bound.session, "child");
    assert_eq!(bound.chain, vec!["child".to_string()]);

    fixture.host.swansong().shut_down().await;
}

/// An approve of an approve is never the thing authorized (docs/permissions.md): the
/// resolver follows the held approval through the host to the leaf, accumulating the
/// chain of participating sessions.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_approve_of_an_approve_carries_a_two_hop_chain() {
    let fixture = Fixture::new();
    let grandchild = fixture.host.session("grandchild").unwrap();
    let leaf = ToolCall::new(
        "bash",
        "toolu_leaf",
        serde_json::json!({ "command": "cargo test" }),
    );
    let _leaf_gate = grandchild.register_pending(&leaf, None);

    let inner = ToolCall::new(
        crate::control_tools::APPROVE_WIRE_NAME,
        "toolu_inner",
        serde_json::json!({ "session": "grandchild", "call_id": "toolu_leaf" }),
    );
    let child = fixture.host.session("child").unwrap();
    let _inner_gate = child.register_pending(&inner, None);

    let outer = ToolCall::new(
        crate::control_tools::APPROVE_WIRE_NAME,
        "toolu_outer",
        serde_json::json!({ "session": "child", "call_id": "toolu_inner" }),
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

/// A `call_id` that is not the target's pending gate refuses with "gate moved" before
/// any classification (docs/permissions.md), and a target with no gate at all refuses
/// the same way.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_stale_or_absent_gate_is_refused() {
    let fixture = Fixture::new();
    let child = fixture.host.session("child").unwrap();
    let _gate = child.register_pending(
        &ToolCall::new("bash", "toolu_real", serde_json::json!({ "command": "ls" })),
        None,
    );

    let stale = ToolCall::new(
        crate::control_tools::APPROVE_WIRE_NAME,
        "toolu_parent",
        serde_json::json!({ "session": "child", "call_id": "toolu_stale" }),
    );
    let err = fixture
        .control("parent")
        .resolve_bound_action(&stale)
        .unwrap_err()
        .to_string();
    assert!(err.contains("gate moved"), "{err}");

    // A descendant with no gate open at all (the tree is parent → child → grandchild).
    let absent = ToolCall::new(
        crate::control_tools::APPROVE_WIRE_NAME,
        "toolu_parent",
        serde_json::json!({ "session": "grandchild", "call_id": "toolu_whatever" }),
    );
    let err = fixture
        .control("parent")
        .resolve_bound_action(&absent)
        .unwrap_err()
        .to_string();
    assert!(err.contains("gate moved"), "{err}");

    fixture.host.swansong().shut_down().await;
}

/// The target rules the Host arm enforces apply to the binding too (docs/permissions.md):
/// self is never a target, and a mutating call reaches descendants only.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_target_rules_apply_to_a_bound_approve() {
    let fixture = Fixture::new();
    let control = fixture.control("parent");

    let own = ToolCall::new(
        crate::control_tools::APPROVE_WIRE_NAME,
        "toolu_parent",
        serde_json::json!({ "session": "parent", "call_id": "x" }),
    );
    let err = control.resolve_bound_action(&own).unwrap_err().to_string();
    assert!(err.contains("cannot target itself"), "{err}");

    let peer = ToolCall::new(
        crate::control_tools::APPROVE_WIRE_NAME,
        "toolu_parent",
        serde_json::json!({ "session": "peer", "call_id": "x" }),
    );
    let err = control.resolve_bound_action(&peer).unwrap_err().to_string();
    assert!(err.contains("neither created nor supervises"), "{err}");

    fixture.host.swansong().shut_down().await;
}

/// In-session `approve` requires the held call's id (docs/tools.md): without one there
/// is no gate to bind, so the resolver refuses rather than guessing.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_bound_approve_without_a_call_id_is_refused() {
    let fixture = Fixture::new();
    let approve = ToolCall::new(
        crate::control_tools::APPROVE_WIRE_NAME,
        "toolu_parent",
        serde_json::json!({ "session": "child" }),
    );
    let err = fixture
        .control("parent")
        .resolve_bound_action(&approve)
        .unwrap_err()
        .to_string();
    assert!(err.contains("requires `call_id`"), "{err}");

    fixture.host.swansong().shut_down().await;
}

/// A session creates only inside its own project (docs/tools.md): a cwd whose
/// `.efference/` base is not the caller's is refused before anything is minted,
/// since no later verb could reach the child. A cwd inside the project passes
/// this check (and fails later here, on the empty inference registry).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_create_outside_the_project_is_refused() {
    let fixture = Fixture::new();
    let control = fixture.control("parent");
    let create = |cwd: &str| Create {
        cwd: Some(cwd.into()),
        name: Some("offshoot".into()),
        model: None,
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
        .err()
        .expect("another project's cwd is refused")
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
        .err()
        .expect("the empty registry refuses the model")
        .to_string();
    assert!(!err.contains("outside this project"), "{err}");

    // A cwd inside one of the project's worktrees belongs to the project, as it does
    // for a caller working in one (docs/worktrees.md).
    let err = create("./.efference/worktrees/w/src")
        .execute(control.clone(), RequestContext::default())
        .await
        .err()
        .expect("the empty registry refuses the model")
        .to_string();
    assert!(!err.contains("outside this project"), "{err}");

    fixture.host.swansong().shut_down().await;
}

/// A succession (docs/tools.md): the successor's first turn is the brief under the
/// reserved hand-off source, the predecessor names its successor for the clients to
/// follow, and the successor — alone among non-descendants — may archive it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_successor_gets_the_brief_and_may_archive_its_predecessor() {
    let fixture = Fixture::new();
    fixture
        .host
        .record_succession("parent", "peer", "carry on".into());

    let parent = fixture.host.session("parent").unwrap();
    assert_eq!(
        parent.identity().and_then(|identity| identity.successor),
        Some("peer".to_string())
    );
    let peer = fixture.host.session("peer").unwrap();
    wait_for("the brief", || {
        peer.snapshot().iter().any(|entry| {
            entry.source.as_deref() == Some("handoff[parent]")
                && matches!(&entry.payload, crate::log::Payload::User(_))
        })
    })
    .await;

    // A session that is neither a descendant nor the successor is refused.
    let err = Archive {
        session: "parent".into(),
    }
    .execute(fixture.control("stranger"), RequestContext::default())
    .await
    .unwrap_err()
    .to_string();
    assert!(
        err.contains("no session") || err.contains("neither created nor supervises"),
        "{err}"
    );

    Archive {
        session: "parent".into(),
    }
    .execute(fixture.control("peer"), RequestContext::default())
    .await
    .expect("the successor archives its predecessor");
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
            serde_json::json!({ "session": target }),
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
    let turn = crate::log::ToolCall::new(
        "sessions__turn",
        "toolu_2",
        serde_json::json!({ "session": "parent" }),
    );
    assert!(!toolbox_for("peer").completes_hand_off(&turn));
    // No sessions provider mounted: nothing clears.
    assert!(!crate::tools::Toolbox::bash_only().completes_hand_off(&archive("parent")));

    fixture.host.swansong().shut_down().await;
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
