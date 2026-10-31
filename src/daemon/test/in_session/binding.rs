//! An allowing `agents__decide` bound to the leaf it authorizes, and the target rules
//! the binding obeys (docs/permissions.md, docs/tools.md).

use super::*;

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
    let _gate = child.register_pending(&leaf, None, None);

    let decide = ToolCall::new(
        crate::protocol::agents::DECIDE_WIRE_NAME,
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
    let _leaf_gate = grandchild.register_pending(&leaf, None, None);

    let inner = ToolCall::new(
        crate::protocol::agents::DECIDE_WIRE_NAME,
        "toolu_inner",
        serde_json::json!({
            "agent": "grandchild", "seq": leaf_seq.to_string(), "decision": "allow"
        }),
    );
    let child = fixture.session("child");
    let inner_seq = issue(&child, vec![inner.clone()]);
    let _inner_gate = child.register_pending(&inner, None, None);

    let outer = ToolCall::new(
        crate::protocol::agents::DECIDE_WIRE_NAME,
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
    let _gate = child.register_pending(&real, None, None);
    let decide = |agent: &str, seq: Option<String>| {
        let mut input = serde_json::json!({ "agent": agent, "decision": "allow" });
        if let Some(seq) = seq {
            input["seq"] = seq.into();
        }
        ToolCall::new(
            crate::protocol::agents::DECIDE_WIRE_NAME,
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
    // A supervised session with no gate open at all: `child` supervises `grandchild`,
    // which it created.
    let err = fixture
        .control("child")
        .resolve_bound_action(&decide("grandchild", Some("3".into())))
        .unwrap_err()
        .to_string();
    assert!(err.contains("gate moved"), "{err}");
    let err = refused(decide("child", None));
    assert!(err.contains("takes `seq`"), "{err}");

    fixture.host.swansong().shut_down().await;
}

/// The target rules the verb enforces apply to the binding too (docs/permissions.md):
/// self is never a target, a decision reaches only the sessions the caller supervises —
/// not a peer, nor a grandchild it drives through the creation chain — and each hop of
/// a chain must be supervised by the session before it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_target_rules_apply_to_a_bound_decide() {
    let fixture = Fixture::new();
    let control = fixture.control("parent");
    let decide = |agent: &str, seq: &str| {
        ToolCall::new(
            crate::protocol::agents::DECIDE_WIRE_NAME,
            "toolu_parent",
            serde_json::json!({ "agent": agent, "seq": seq, "decision": "allow" }),
        )
    };
    let err = control
        .resolve_bound_action(&decide("parent", "1"))
        .unwrap_err()
        .to_string();
    assert!(err.contains("a session cannot address itself"), "{err}");
    let err = control
        .resolve_bound_action(&decide("peer", "1"))
        .unwrap_err()
        .to_string();
    assert_eq!(
        err,
        "refused: you don't supervise \"peer\", so its gates are not yours to decide — it \
         has no supervisor, so the operator decides them; agents__message leaves it a note"
    );
    let err = control
        .resolve_bound_action(&decide("grandchild", "1"))
        .unwrap_err()
        .to_string();
    assert_eq!(
        err,
        "refused: you don't supervise \"grandchild\", so its gates are not yours to decide — \
         its supervisor, \"child\", and the operator decide them; agents__message and \
         agents__interrupt still reach it"
    );

    // `child` holds an allowing decision of `peer`, which it does not supervise: the
    // hop is refused before anything is bound.
    let child = fixture.session("child");
    let inner = decide("peer", "1");
    let inner_seq = issue(&child, vec![inner.clone()]);
    let _gate = child.register_pending(&inner, None, None);
    let err = control
        .resolve_bound_action(&decide("child", &inner_seq.to_string()))
        .unwrap_err()
        .to_string();
    assert!(
        err.starts_with("refused: \"child\" does not supervise \"peer\""),
        "{err}"
    );

    fixture.host.swansong().shut_down().await;
}
