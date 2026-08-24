//! Tests for `entry_view.rs`.

use super::*;
use crate::log::{ContentBlock, Timestamp, WireState};

fn entry(seq: u64, submission_id: Option<&str>, payload: Payload) -> LogEntry {
    LogEntry {
        seq,
        turn: 1,
        at: Timestamp(1_700_000_000_000),
        submission_id: submission_id.map(String::from),
        payload,
    }
}

fn result_with_images(images: Vec<ResultImage>) -> Payload {
    Payload::ToolResult {
        call_id: CallId("toolu_img".into()),
        raw: RawResult {
            content: "1×1 image/webp, 1 visual tokens".into(),
            is_error: false,
            images,
        },
        wire: WireState::Raw,
    }
}

/// The mirror pin: an image-free entry serializes byte-identically through the view
/// and the derived path, across every payload shape the view touches or delegates.
/// A field added to `LogEntry`/`RawResult` without updating `entry_view.rs` fails here.
#[test]
fn view_of_an_image_free_entry_is_the_derived_bytes() {
    let cases = [
        entry(
            0,
            None,
            Payload::User(vec![ContentBlock::Text("hi".into())]),
        ),
        entry(
            1,
            Some("sub-1"),
            Payload::Assistant {
                thinking: vec![],
                text: vec![ContentBlock::Text("ok".into())],
                calls: vec![ToolCall {
                    id: CallId("toolu_1".into()),
                    name: "bash".into(),
                    input: serde_json::json!({ "command": "ls" }),
                }],
                upstream_failure: None,
            },
        ),
        entry(2, None, result_with_images(Vec::new())),
        entry(
            3,
            None,
            Payload::ToolResult {
                call_id: CallId("toolu_2".into()),
                raw: RawResult {
                    content: "gone".into(),
                    is_error: true,
                    images: Vec::new(),
                },
                wire: WireState::Elided,
            },
        ),
    ];
    for case in &cases {
        assert_eq!(
            serde_json::to_string(&EntryView(case)).unwrap(),
            serde_json::to_string(case).unwrap(),
            "diverged on seq {}",
            case.seq
        );
    }
}

/// The one deliberate divergence: `data` is omitted from a result image; the layout
/// metadata survives. Everything else in the entry is untouched.
#[test]
fn view_strips_image_data_and_keeps_layout_metadata() {
    let case = entry(
        5,
        None,
        result_with_images(vec![ResultImage {
            media_type: "image/webp".into(),
            data: "AAAA".into(),
            width: 40,
            height: 30,
        }]),
    );
    let view = serde_json::to_string(&EntryView(&case)).unwrap();
    assert!(
        !view.contains("AAAA"),
        "base64 must not ride the view: {view}"
    );

    // Field order in the serialized bytes (a parsed `Value` re-sorts keys).
    assert!(
        view.contains(r#"{"media_type":"image/webp","width":40,"height":30}"#),
        "the reference shape, in declaration order: {view}"
    );
    let parsed: serde_json::Value = serde_json::from_str(&view).unwrap();
    let image = &parsed["payload"]["ToolResult"]["raw"]["images"][0];
    assert!(image.get("data").is_none(), "data omitted, not emptied");
    assert_eq!(image["media_type"], "image/webp");
    assert_eq!(image["width"], 40);
    assert_eq!(image["height"], 30);
}

/// A stripped view round-trips into the shared [`LogEntry`] type — `data` reads back
/// empty via its serde default, so a Rust client parses the view with the log's own
/// types rather than a parallel set.
#[test]
fn stripped_view_deserializes_as_a_log_entry() {
    let case = entry(
        6,
        None,
        result_with_images(vec![ResultImage {
            media_type: "image/png".into(),
            data: "BBBB".into(),
            width: 2,
            height: 2,
        }]),
    );
    let view = serde_json::to_string(&EntryView(&case)).unwrap();
    let parsed: LogEntry = serde_json::from_str(&view).expect("view parses as LogEntry");
    let Payload::ToolResult { raw, .. } = &parsed.payload else {
        panic!("payload variant survived");
    };
    assert_eq!(raw.images.len(), 1);
    assert_eq!(raw.images[0].media_type, "image/png");
    assert_eq!(raw.images[0].data, "", "absent data reads back empty");
}
