use super::*;
use crate::daemon::entry_view::MAX_INLINE_RESULT_BYTES;
use crate::log::{ContentBlock, RawResult, WireState};

fn one_entry(payload: Payload) -> LogEntry {
    let mut log = Log::in_memory();
    log.append(1, payload).expect("in-memory append");
    log.entries()[0].clone()
}

fn user(text: &str) -> Payload {
    Payload::User(vec![ContentBlock::Text(text.into())])
}

/// A verdict frame states the line counts of the result it decides, read through the
/// session the tail already holds — the one entry that carries no bytes of its own.
/// The bytes a client sees must be the `/log` element's, counts included, or a tail
/// and a catch-up read would disagree about the same result.
#[test]
fn a_verdict_frame_carries_the_counts_of_the_result_it_names() {
    let mut log = Log::in_memory();
    let content = (1..=40)
        .map(|i| format!("line {i}"))
        .collect::<Vec<_>>()
        .join("\n");
    log.append(
        1,
        Payload::tool_result("toolu_1", RawResult::text(&content), WireState::Pending),
    )
    .expect("in-memory append");
    log.append(
        1,
        Payload::ResultVerdict {
            for_result: 0,
            wire: WireState::from(crate::log::Distilled::new(crate::log::Keep::Ranges(vec![
                (1, 6),
            ]))),
        },
    )
    .expect("in-memory append");
    let entries = log.entries().to_vec();
    let (shared, _turns_rx) = Session::new(std::env::current_dir().unwrap(), log);

    // What the tail resolves for this entry, and what the `/log` window serves for it.
    let resolved = crate::daemon::stream::result_raw(&entries[1], &shared);
    let frame = Frame::log(&entries[1], resolved.as_ref());
    let window: Vec<Value> =
        serde_json::from_str(&shared.log_json(&LogSelect::default())).expect("a JSON array");

    let counts = &window[1]["payload"]["ResultVerdict"]["distillate"];
    assert_eq!(counts["total_lines"], 40);
    assert_eq!(counts["kept_lines"], 6);
    // Byte-identity is with the view the `/log` route serializes; a `Value`
    // round-trip would re-sort the keys and lose it (entry_view.rs).
    assert_eq!(
        frame.data.get(),
        serde_json::to_string(&EntryView::new(&entries[1]).with_result_raw(resolved.as_ref()))
            .unwrap(),
        "a log frame's data is the /log element verbatim"
    );
    assert_eq!(
        serde_json::from_str::<Value>(frame.data.get()).unwrap(),
        window[1],
        "the tail and a catch-up read agree about the same result"
    );
}

/// The `log` class is the only one carrying a cursor, and its `data` must stay
/// byte-identical to the corresponding `GET /log` element — the property `sse.rs`
/// relies on so the tail and the catch-up read agree per entry.
///
/// **The pin is bounded:** identity holds *below*
/// [`MAX_INLINE_RESULT_BYTES`]. Above it a frame and a window element still agree
/// with each other — both are `EntryView::new` — but both diverge from
/// `GET /log/:seq`, which serves the whole result. See the module docs; the two
/// sides of the threshold are pinned below.
#[test]
fn log_frames_carry_seq_and_data_matching_the_log_bytes() {
    let entry = one_entry(user("hello"));
    let frame = Frame::log(&entry, None);

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

    let bare = Frame::log(&entry, None).to_json();
    assert!(!bare.contains("render"), "no render key when unset: {bare}");

    let rendered = Frame::log(&entry, None).with_render(&serde_json::json!({"blocks": []}));
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
        Topic::Stall,
        Topic::UpstreamFailure,
        Topic::UpstreamRetry,
        Topic::Cold,
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
            "stall",
            "upstream_failure",
            "upstream_retry",
            "cold",
            "session_identity",
        ]
    );
}

/// The relaxed pin, both sides. A frame's `data` is the window element for the same
/// entry, threshold or no threshold — `Frame::log` and `log_json` call the same
/// constructor — and the divergence is only ever against the entry route.
#[test]
fn a_log_frame_matches_the_window_and_defers_to_the_entry_route_when_oversized() {
    let small = one_entry(Payload::tool_result(
        "toolu_small",
        RawResult::text("small"),
        WireState::Raw,
    ));
    let frame = Frame::log(&small, None);
    assert_eq!(
        frame.data.get(),
        serde_json::to_string(&crate::daemon::entry_view::EntryView::new(&small)).unwrap(),
        "the frame is the window element"
    );
    assert_eq!(
        frame.data.get(),
        serde_json::to_string(&crate::daemon::entry_view::EntryView::full(&small)).unwrap(),
        "below the threshold the entry route is the same bytes"
    );
    assert!(!frame.data.get().contains("content_elided"));

    let big = one_entry(Payload::tool_result(
        "toolu_big",
        RawResult::text("z".repeat(MAX_INLINE_RESULT_BYTES * 2)),
        WireState::Raw,
    ));
    let frame = Frame::log(&big, None);
    assert_eq!(
        frame.data.get(),
        serde_json::to_string(&crate::daemon::entry_view::EntryView::new(&big)).unwrap(),
        "the frame is still the window element"
    );
    assert!(
        frame.data.get().contains(&format!(
            r#""content_elided":{{"kept_bytes":{MAX_INLINE_RESULT_BYTES},"total_bytes":{}}}"#,
            MAX_INLINE_RESULT_BYTES * 2
        )),
        "the tag rides the frame in declaration order: {}",
        &frame.data.get()[..200]
    );
    assert!(
        frame.data.get().len() < serde_json::to_string(&big).unwrap().len(),
        "and the whole result stayed off the stream"
    );
}
