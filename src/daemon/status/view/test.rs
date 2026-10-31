//! The status view as the host builds it from a session's state.

use super::*;
use crate::log::{AssistantOutput, Axes, ContentBlock, Precedent, Stance, Telemetry};
use crate::persist::Log;

fn session() -> Arc<Session> {
    Session::new(std::env::current_dir().unwrap(), Log::in_memory()).0
}

fn assistant(text: &[&str], calls: Vec<ToolCall>) -> Payload {
    Payload::Assistant {
        output: AssistantOutput::from_legacy_messages(
            vec![],
            text.iter()
                .map(|text| ContentBlock::Text(text.to_string()))
                .collect(),
            calls,
        ),
        excluded: None,
    }
}

/// A held call carries the handle the transcript gives it, what it does, and why it
/// was held in plain words, read from the decision rung's recorded answers; with no
/// live supervisor the operator decides it.
#[test]
fn a_gate_names_its_handle_summary_and_plain_reason() {
    let shared = session();
    let read = ToolCall::new(
        "fs__read",
        "toolu_0",
        serde_json::json!({ "path": "a.rs", "intent": "read it" }),
    );
    let curl = ToolCall::new(
        crate::bash::NAME,
        "toolu_1",
        serde_json::json!({ "command": "curl -sI https://example.com", "intent": "fetch headers" }),
    );
    shared
        .append(1, assistant(&[], vec![read, curl.clone()]))
        .unwrap();
    shared
        .append(
            1,
            Payload::Spot(Spot {
                call_id: CallId("toolu_1".into()),
                stance: Stance::Gate,
                reason: "level 2 (scope fit, 21%): needs 1 approved call like it, found none."
                    .into(),
                origin: Origin::Model,
                model: None,
                probed_prefix: None,
                axes: Some(Axes {
                    in_scope: 0.21,
                    precedent: Some(Precedent {
                        candidates: 0,
                        same: 0,
                        wider: 0,
                        required: Some(1),
                        compared: Vec::new(),
                    }),
                    ..Axes::default()
                }),
            }),
        )
        .unwrap();
    shared.set_phase(LoopPhase::Executing);
    let _rx = shared.register_pending(&curl, None, None);

    let view = shared.status_view("child", &shared.status());
    assert_eq!(
        view.condition,
        Condition::Gated(HeldCall {
            call_id: "toolu_1".into(),
            tool: "bash".into(),
            summary: Some("curl -sI https://example.com".into()),
            intent: Some("fetch headers".into()),
            handle: Some("0.2".into()),
            held_because: Some(
                "its reviewer is unsure this fits the session's scope; no earlier call like it \
                 was allowed"
                    .into()
            ),
            reviewer: Reviewer::Operator,
            escalations: Vec::new(),
        })
    );
}

/// The closing message is whole up to the cap, cut on a char boundary past it, with the
/// whole message's size and lines kept so the cut can be stated.
#[test]
fn the_closing_message_is_capped_on_a_char_boundary() {
    let shared = session();
    shared
        .append(1, assistant(&["first paragraph", "second"], vec![]))
        .unwrap();
    let Condition::Idle {
        closing: Some(closing),
    } = shared.status_view("child", &SessionStatus::Idle).condition
    else {
        panic!("an idle status with a closing message");
    };
    assert_eq!(
        closing,
        Closing {
            seq: 0,
            text: "first paragraph\n\nsecond".into(),
            bytes: 23,
            lines: 3,
        }
    );

    // Three bytes a char, so the cap falls inside one.
    let long = "€".repeat(2000);
    shared.append(2, assistant(&[&long], vec![])).unwrap();
    let Condition::Idle {
        closing: Some(closing),
    } = shared.status_view("child", &SessionStatus::Idle).condition
    else {
        panic!("an idle status with a closing message");
    };
    assert_eq!(closing.seq, 1);
    assert_eq!(closing.text.len(), CLOSING_CAP - CLOSING_CAP % 3);
    assert_eq!(closing.bytes, 6000);
}

/// A running tool is what a working session is doing, named as its display name with a
/// `bash` call's command.
#[test]
fn a_running_tool_is_the_working_activity() {
    let shared = session();
    let test = ToolCall::new(
        crate::bash::NAME,
        "toolu_1",
        serde_json::json!({ "command": "cargo test", "intent": "run the suite" }),
    );
    shared.append(1, assistant(&[], vec![test])).unwrap();
    shared
        .append(
            1,
            Payload::Telemetry(Telemetry::CallStarted {
                call_id: CallId("toolu_1".into()),
            }),
        )
        .unwrap();
    shared.set_phase(LoopPhase::Executing);
    let Condition::Working { activity } = shared.status_view("child", &shared.status()).condition
    else {
        panic!("a working status");
    };
    assert!(
        matches!(
            activity.as_slice(),
            [Activity::Tool { tool, command: Some(command), .. }]
                if tool == "bash" && command == "cargo test"
        ),
        "{activity:?}"
    );
}

/// The topic is the view of the status now, under the session's id.
#[test]
fn the_status_topic_is_the_view_now() {
    let shared = session();
    let state: Value = serde_json::from_str(&shared.state_json()).unwrap();
    assert_eq!(state["status"]["condition"]["kind"], "idle");
    let call = ToolCall::new("bash", "toolu_1", serde_json::json!({ "command": "ls" }));
    shared.set_phase(LoopPhase::Executing);
    let _rx = shared.register_pending(&call, None, None);
    let state: Value = serde_json::from_str(&shared.state_json()).unwrap();
    assert_eq!(state["status"]["condition"]["kind"], "gated");
    assert_eq!(state["status"]["condition"]["summary"], "ls");
    assert_eq!(state["status"]["condition"]["reviewer"]["kind"], "operator");
}

/// The view survives the trip through its serialized form, which is what a client of
/// `GET /state` renders from.
#[test]
fn the_view_reads_back_from_its_json() {
    let shared = session();
    shared.append(1, assistant(&["done"], vec![])).unwrap();
    let view = shared.status_view("child", &SessionStatus::Idle);
    let json = serde_json::to_string(&view).unwrap();
    assert_eq!(serde_json::from_str::<StatusView>(&json).unwrap(), view);
}
