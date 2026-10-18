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
                model: None,
                probed_prefix: None,
                axes: None,
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

/// A bound `agents__decide` gate surfaces the child's held action on `/state`
/// (docs/permissions.md): `acting_on` carries the leaf tool, its input, its intent,
/// and the session chain, so a client renders the move rather than the approval's id.
#[test]
fn the_gate_carries_the_bound_leaf_action() {
    let (shared, _rx) = Session::new(std::env::current_dir().unwrap(), Log::in_memory());
    let approve = ToolCall::new(
        crate::control_tools::DECIDE_WIRE_NAME,
        "toolu_parent",
        serde_json::json!({ "agent": "child", "seq": "4", "decision": "allow" }),
    );
    let bound = crate::control_tools::BoundAction {
        name: "bash".into(),
        input: serde_json::json!({ "command": "rm -rf build", "intent": "clean the tree" }),
        intent: Some("clean the tree".into()),
        cwd: std::path::PathBuf::from("/work"),
        project: std::path::PathBuf::from("/work"),
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

/// The `GateDecision` records in `session`'s log, in order.
fn decisions(session: &Session) -> Vec<crate::log::GateDecision> {
    session
        .snapshot()
        .into_iter()
        .filter_map(|e| match e.payload {
            Payload::Telemetry(crate::log::Telemetry::GateDecision(decision)) => Some(decision),
            _ => None,
        })
        .collect()
}

/// `PUT /approvals/:call_id` records each decision it accepts with its decider
/// (docs/control-api.md): the body's `source`, or none for the operator. A decision on a
/// call that is not the pending gate records nothing.
#[tokio::test]
async fn a_decision_over_http_is_recorded_with_its_decider() {
    let (shared, _rx) = Session::new(std::env::current_dir().unwrap(), Log::in_memory());
    let server = test_server(&shared).await;

    let _gate = shared.register_pending(&bash_call("ls"), None);
    server
        .put("/v1/approvals/toolu_x")
        .with_body(r#"{"decision":"approve","source":"web"}"#)
        .await
        .assert_status(200);
    let _gate = shared.register_pending(&bash_call("rm -rf tmp"), None);
    server
        .put("/v1/approvals/toolu_x")
        .with_body(r#"{"decision":"redirect","message":"use trash"}"#)
        .await
        .assert_status(200);
    server
        .put("/v1/approvals/toolu_x")
        .with_body(r#"{"decision":"abort","source":"web"}"#)
        .await
        .assert_status(409);

    assert_eq!(
        decisions(&shared),
        [
            crate::log::GateDecision::new("toolu_x", Approval::Approve, Some("web".into())),
            crate::log::GateDecision::new("toolu_x", Approval::Redirect("use trash".into()), None),
        ]
    );
    let entry = shared
        .snapshot()
        .into_iter()
        .find(|e| {
            matches!(
                e.payload,
                Payload::Telemetry(crate::log::Telemetry::GateDecision(_))
            )
        })
        .unwrap();
    assert_eq!(
        serde_json::to_string(&entry.payload).unwrap(),
        r#"{"Telemetry":{"GateDecision":{"call_id":"toolu_x","decision":"Approve","source":"web"}}}"#
    );
}

/// The conversation names the decider for the reader (docs/control-api.md): the
/// operator's own decision reads "operator", the reader's own reads "you".
#[tokio::test]
async fn the_transcript_names_who_decided() {
    let (shared, _rx) = Session::new(std::env::current_dir().unwrap(), Log::in_memory());
    let call = bash_call("ls").with_intent("look around");
    shared
        .append(
            1,
            Payload::Assistant {
                output: crate::log::AssistantOutput::from_legacy_messages(
                    vec![],
                    vec![],
                    vec![call.clone()],
                ),
                excluded: None,
            },
        )
        .unwrap();
    let server = test_server(&shared).await;
    let _gate = shared.register_pending(&call, None);
    let held = server.get("/v1/transcript").await;
    held.assert_ok();
    assert_eq!(
        held.body(),
        "transcript, seq 0–1\n0  bash `ls` — look around → waiting for a decision\n"
    );

    server
        .put("/v1/approvals/toolu_x")
        .with_body(r#"{"decision":"approve","source":"mcp[claude-code]"}"#)
        .await
        .assert_status(200);
    let read = server.get("/v1/transcript?me=mcp%5Bclaude-code%5D").await;
    read.assert_ok();
    assert_eq!(
        read.body(),
        "transcript, seq 0–2\n0  bash `ls` — look around → allowed by you → running\n"
    );
}
