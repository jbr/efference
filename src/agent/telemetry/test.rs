use super::*;
use crate::log::Telemetry;

use crate::log::CallId;
use serde_json::json;

fn call(name: &str, input: serde_json::Value) -> ToolCall {
    ToolCall::new(name, "toolu_1", input)
}

fn result(content: &str, is_error: bool) -> RawResult {
    RawResult::new(content, is_error)
}

const INSTANT: std::time::Duration = std::time::Duration::from_millis(12);

/// The three outcome classes are readable off every tool without per-tool
/// knowledge (docs/tools.md): `is_error` carries refusal, and the
/// caller's `harness_error` is the only thing that distinguishes
/// "the harness could not run this" from "the tool ran and declined".
#[test]
fn outcome_classes_derive_generically() {
    let ok = observe(
        &call("read", json!({})),
        &result("body", false),
        INSTANT,
        false,
        None,
    );
    assert_eq!(ok.outcome, crate::log::ToolOutcome::Applied);
    let refused = observe(
        &call("edit", json!({})),
        &result("anchor matched nothing", true),
        INSTANT,
        false,
        None,
    );
    assert_eq!(refused.outcome, crate::log::ToolOutcome::Rejected);
    let broken = observe(
        &call("bash", json!({})),
        &result("failed to background: no pty", true),
        INSTANT,
        true,
        None,
    );
    assert_eq!(broken.outcome, crate::log::ToolOutcome::HarnessError);
}

/// The common fields are the same four for every tool, and `result_bytes` is the
/// pre-distillation size — the number the gate's saving is measured against.
#[test]
fn common_fields_are_populated_for_any_tool() {
    let t = observe(
        &call("ferritin__get", json!({"path": "std::vec::Vec"})),
        &result("a much longer body", false),
        std::time::Duration::from_millis(1500),
        false,
        None,
    );
    assert_eq!(t.tool, "ferritin__get");
    assert_eq!(t.call_id, CallId("toolu_1".into()));
    assert_eq!(t.duration_ms, 1500);
    assert_eq!(t.result_bytes, "a much longer body".len());
    assert_eq!(t.images, 0);
    assert!(t.bash.is_none(), "not a bash call");
    assert!(t.edit.is_none(), "no anchor or content argument");
}

/// A bash call records the status its own result text reports, read back through
/// the parser that sits beside the writer. `None` on a clean exit is the
/// deliberate spelling: the result reports no status there.
#[test]
fn bash_exit_status_round_trips_through_the_result_text() {
    let clean = observe(
        &call("bash", json!({"command": "true"})),
        &result("(no output)", false),
        INSTANT,
        false,
        None,
    );
    let bash = clean.bash.expect("a bash call records bash fields");
    assert_eq!(bash.exit_status, None);
    assert!(!bash.background);

    let failed = observe(
        &call("bash", json!({"command": "false"})),
        &result("some output\n[exited with status 1]", true),
        INSTANT,
        false,
        None,
    );
    assert_eq!(
        failed.bash.expect("bash fields").exit_status.as_deref(),
        Some("1")
    );
    let killed = observe(
        &call("bash", json!({"command": "sleep 99"})),
        &result("[exited with status signal]", true),
        INSTANT,
        false,
        None,
    );
    assert_eq!(
        killed.bash.expect("bash fields").exit_status.as_deref(),
        Some("signal"),
        "a signal is recorded as the token, not squeezed into an integer"
    );

    let backgrounded = observe(
        &call(
            "bash",
            json!({"command": "cargo watch", "background": true}),
        ),
        &result("backgrounded as p#1", false),
        INSTANT,
        false,
        Some("p#1".to_string()),
    );
    let bash = backgrounded.bash.expect("bash fields");
    assert!(bash.background);
    assert_eq!(
        bash.handle.as_deref(),
        Some("p#1"),
        "the minted handle is a field, not a string to parse back out of the result"
    );
}

/// Edit fields come off the call's own arguments, under any of the spellings the
/// mounted editing tools use — nothing is plumbed out of the tool crates.
#[test]
fn edit_fields_come_from_the_call_arguments() {
    let t = observe(
        &call(
            "semantic_edit",
            json!({"operation": "replace", "anchor": "fn main", "content": "fn main() {}"}),
        ),
        &result("Applied", false),
        INSTANT,
        false,
        None,
    );
    let edit = t.edit.expect("an edit-shaped call records edit fields");
    assert_eq!(edit.operation.as_deref(), Some("replace"));
    assert_eq!(edit.anchor_bytes, Some("fn main".len()));
    assert_eq!(edit.content_bytes, Some("fn main() {}".len()));

    // The fs-tools spelling of the same two arguments.
    let alt = observe(
        &call("write", json!({"old_string": "abc", "new_string": "abcd"})),
        &result("ok", false),
        INSTANT,
        false,
        None,
    );
    let edit = alt.edit.expect("recognized under the other spelling");
    assert_eq!(edit.anchor_bytes, Some(3));
    assert_eq!(edit.content_bytes, Some(4));
    assert_eq!(edit.operation, None);
}

/// A telemetry entry round-trips through the log's serialization, and the
/// optional halves are **absent** rather than null when they do not apply — the
/// per-call cost of recording this on every call is what keeps it affordable.
#[test]
fn telemetry_serializes_without_its_absent_halves() {
    let t = observe(
        &call("read", json!({"path": "x"})),
        &result("body", false),
        INSTANT,
        false,
        None,
    );
    let line = serde_json::to_string(&crate::log::Payload::Telemetry(Telemetry::Tool(t))).unwrap();
    assert!(!line.contains("bash"), "{line}");
    assert!(!line.contains("edit"), "{line}");
    assert!(!line.contains("images"), "{line}");
    assert!(line.contains(r#""outcome":"applied""#), "{line}");
    let back: crate::log::Payload = serde_json::from_str(&line).unwrap();
    assert!(matches!(
        back,
        crate::log::Payload::Telemetry(Telemetry::Tool(_))
    ));
}
