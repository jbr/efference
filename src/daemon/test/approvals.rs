//! Tests for `approvals.rs`.

use super::*;

/// A gate surfaces the spotter's reason on `/state` (docs/permissions.md): the spotter logs
/// its verdict as a `Spot` *before* routing to the human, so `register_pending` reads the
/// reason from the log — no side channel. The client renders it inline at the gate.
#[test]
fn the_gate_carries_the_spotter_reason() {
    let (shared, _rx) = Session::new(std::env::current_dir().unwrap(), Log::in_memory());
    let call = bash_call("rm -rf build");
    shared
        .append(
            0,
            Payload::Spot(crate::log::Spot {
                call_id: call.id.clone(),
                stance: crate::log::Stance::Gate,
                reason: "deletes outside the git tree — not reversible".into(),
                origin: crate::log::Origin::Model,
                probed_prefix: None,
            }),
        )
        .unwrap();
    let _gate = shared.register_pending(&call, None);
    assert!(
        shared
            .state_json()
            .contains("deletes outside the git tree — not reversible"),
        "the gate's /state should carry the spotter's reason",
    );
}

/// A bound `sessions__approve` gate surfaces the child's held action on `/state`
/// (docs/permissions.md): `acting_on` carries the leaf tool, its input, its intent,
/// and the session chain, so a client renders the move rather than the approval's id.
#[test]
fn the_gate_carries_the_bound_leaf_action() {
    let (shared, _rx) = Session::new(std::env::current_dir().unwrap(), Log::in_memory());
    let approve = ToolCall::new(
        crate::control_tools::APPROVE_WIRE_NAME,
        "toolu_parent",
        serde_json::json!({ "session": "child", "call_id": "toolu_leaf" }),
    );
    let bound = crate::control_tools::BoundAction {
        name: "bash".into(),
        input: serde_json::json!({ "command": "rm -rf build", "intent": "clean the tree" }),
        intent: Some("clean the tree".into()),
        cwd: std::path::PathBuf::from("/work"),
        session: "child".into(),
        chain: vec!["child".into()],
    };
    let _gate = shared.register_pending(&approve, Some(&bound));

    let state = shared.state_json();
    assert!(state.contains("\"acting_on\""), "{state}");
    assert!(state.contains("rm -rf build"), "{state}");
    assert!(state.contains("clean the tree"), "{state}");
    assert!(state.contains("\"child\""), "{state}");
}
