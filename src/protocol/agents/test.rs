use super::*;
use serde_json::json;

/// The control tools' names are in the cached `tools[]` prefix and in every recorded
/// call (docs/log-and-projection.md): renaming one moves the cache of every session
/// that mounts them.
#[test]
fn the_names_on_the_tool_wire_are_fixed() {
    assert_eq!(DECIDE_WIRE_NAME, "agents__decide");
    assert_eq!(HAND_OFF_WIRE_NAME, "agents__hand_off");
    assert_eq!(ARCHIVE_WIRE_NAME, "agents__archive");
}

fn decide(decision: &str) -> ToolCall {
    ToolCall::new(
        DECIDE_WIRE_NAME,
        "toolu_1",
        json!({"agent": "child", "seq": "3", "decision": decision}),
    )
}

/// An allow is bound to the action it authorizes; a deny or an escalate authorizes
/// nothing; anything else is neither (docs/permissions.md).
#[test]
fn a_decide_call_is_read_by_its_decision() {
    assert!(allows(&decide("allow")));
    assert!(!authorizes_nothing(&decide("allow")));
    for decision in ["deny", "escalate"] {
        assert!(!allows(&decide(decision)), "{decision}");
        assert!(authorizes_nothing(&decide(decision)), "{decision}");
    }
    assert!(!allows(&decide("maybe")) && !authorizes_nothing(&decide("maybe")));
    let other = ToolCall::new("bash", "toolu_2", json!({"decision": "allow"}));
    assert!(!allows(&other) && !authorizes_nothing(&other));
}

/// An archive is read by the session it names; any other call, or an archive naming
/// none, has no target (docs/permissions.md).
#[test]
fn an_archive_is_read_by_its_target() {
    let archive = |input| ToolCall::new(ARCHIVE_WIRE_NAME, "toolu_1", input);
    assert_eq!(
        archive_target(&archive(json!({"agent": "kid"}))),
        Some("kid")
    );
    assert_eq!(archive_target(&archive(json!({}))), None);
    assert_eq!(archive_target(&archive(json!({"agent": 3}))), None);
    let message = ToolCall::new("agents__message", "toolu_2", json!({"agent": "kid"}));
    assert_eq!(archive_target(&message), None);
}

/// The bound action rides the gate view as JSON: field order is declaration order, and an
/// absent intent is left out.
#[test]
fn a_bound_action_serializes_in_declaration_order() {
    let mut bound = BoundAction {
        name: "bash".into(),
        input: json!({"command": "ls"}),
        intent: None,
        cwd: "/work".into(),
        project: "/project".into(),
        session: "child".into(),
        chain: vec!["child".into()],
    };
    assert_eq!(
        serde_json::to_string(&bound).unwrap(),
        r#"{"name":"bash","input":{"command":"ls"},"cwd":"/work","project":"/project","session":"child","chain":["child"]}"#
    );
    bound.intent = Some("list the files".into());
    assert_eq!(
        serde_json::to_string(&bound).unwrap(),
        r#"{"name":"bash","input":{"command":"ls"},"intent":"list the files","cwd":"/work","project":"/project","session":"child","chain":["child"]}"#
    );
}
