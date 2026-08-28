use super::*;
use crate::log::ContentBlock;

fn one_entry(payload: Payload) -> LogEntry {
    let mut log = Log::in_memory();
    log.append(1, payload).expect("in-memory append");
    log.entries()[0].clone()
}

fn user(text: &str) -> Payload {
    Payload::User(vec![ContentBlock::Text(text.into())])
}

/// The `log` class is the only one carrying a cursor, and its `data` must stay
/// byte-identical to the corresponding `GET /log` element — the property `sse.rs`
/// relies on so the tail and the catch-up read agree per entry.
#[test]
fn log_frames_carry_seq_and_data_matching_the_log_bytes() {
    let entry = one_entry(user("hello"));
    let frame = Frame::log(&entry);

    assert_eq!(frame.class, Class::Log);
    assert_eq!(frame.kind, "user");
    assert_eq!(frame.seq, Some(entry.seq));

    assert_eq!(
        frame.data.get(),
        serde_json::to_string(&entry).unwrap(),
        "a log frame's data is the /log element verbatim"
    );
}

/// `render` is a sibling of `data`, not a member of it: folding it in would fork the two
/// representations of one entry. It is also absent — not null — when unset, so a client
/// distinguishes "no render document" from "an empty one".
#[test]
fn render_rides_beside_data_and_is_absent_when_unset() {
    let entry = one_entry(user("hello"));

    let bare = Frame::log(&entry).to_json();
    assert!(!bare.contains("render"), "no render key when unset: {bare}");

    let rendered = Frame::log(&entry).with_render(&serde_json::json!({"blocks": []}));
    assert!(
        !rendered.data.get().contains("render"),
        "render must not leak into data: {}",
        rendered.data.get()
    );
    assert_eq!(
        rendered.render.as_ref().map(|r| r.get()),
        Some(r#"{"blocks":[]}"#),
        "render is carried on the frame"
    );
}

/// Ephemeral frames carry no cursor. A `seq` here would be picked up as an SSE `id:` and
/// corrupt the durable resume cursor. The `output` class nests under `data` like every
/// other class, so one parser handles all three.
#[test]
fn output_frames_carry_no_seq_and_nest_under_data() {
    let frame = Frame::output(&StreamDelta::ToolOutput {
        call_id: "toolu_1".into(),
        chunk: "hi".into(),
    });

    assert_eq!(frame.seq, None);
    assert_eq!(
        frame.to_json(),
        r#"{"class":"output","kind":"tool_output","data":{"call_id":"toolu_1","chunk":"hi"}}"#
    );
}

#[test]
fn every_output_delta_maps_to_a_kind_and_no_cursor() {
    let deltas = [
        StreamDelta::AssistantText {
            index: 0,
            text: "a".into(),
        },
        StreamDelta::ThinkingText {
            index: 0,
            text: "t".into(),
        },
        StreamDelta::ToolCallStart {
            index: 1,
            call_id: "toolu_1".into(),
            name: "bash".into(),
        },
        StreamDelta::ToolCallInput {
            index: 1,
            fragment: "{\"c".into(),
        },
        StreamDelta::ToolOutput {
            call_id: "c".into(),
            chunk: "o".into(),
        },
        StreamDelta::ProcessOutput {
            id: 1,
            chunk: "o".into(),
        },
    ];
    let kinds: Vec<&str> = deltas
        .iter()
        .map(|d| {
            let frame = Frame::output(d);
            assert_eq!(frame.seq, None, "an output frame never carries a cursor");
            assert_eq!(frame.class, Class::Output);
            frame.kind
        })
        .collect();
    assert_eq!(
        kinds,
        [
            "assistant_delta",
            "thinking_delta",
            "tool_call_start",
            "tool_call_input",
            "tool_output",
            "process_output",
        ]
    );
}

/// State frames are level-triggered whole-topic replacements, and `null` is a *value* —
/// "this topic is cleared" — not an absent field.
#[test]
fn state_frames_carry_no_seq_and_null_means_cleared() {
    let cleared = Frame::state(Topic::Gate, &Value::Null);
    assert_eq!(cleared.seq, None);
    assert_eq!(
        cleared.to_json(),
        r#"{"class":"state","kind":"gate","data":null}"#
    );

    let held = Frame::state(
        Topic::Pace,
        &serde_json::json!({"speed_ms": 0, "held": true}),
    );
    assert_eq!(
        held.to_json(),
        r#"{"class":"state","kind":"pace","data":{"held":true,"speed_ms":0}}"#
    );
}

/// The topic set is the contract a client writes its state store against; pin the wire
/// names so a rename has to be deliberate.
#[test]
fn topic_wire_names_are_pinned() {
    let names: Vec<&str> = [
        Topic::Gate,
        Topic::Processes,
        Topic::Usage,
        Topic::Role,
        Topic::Proposals,
        Topic::Notifications,
        Topic::Pace,
        Topic::ReviewLevel,
        Topic::Running,
        Topic::UpstreamFailure,
        Topic::UpstreamRetry,
        Topic::SessionIdentity,
    ]
    .iter()
    .map(|t| t.as_str())
    .collect();
    assert_eq!(
        names,
        [
            "gate",
            "processes",
            "usage",
            "role",
            "proposals",
            "notifications",
            "pace",
            "review_level",
            "running",
            "upstream_failure",
            "upstream_retry",
            "session_identity",
        ]
    );
}
