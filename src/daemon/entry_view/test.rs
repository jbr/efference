//! Tests for `entry_view.rs`.

use super::*;
use crate::log::{ContentBlock, ContentElision, Timestamp, WireState};

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
            content_elided: None,
        },
        wire: WireState::Raw,
    }
}

/// The mirror pin: an image-free entry **under the threshold** serializes
/// byte-identically through the view and the derived path, across every payload shape
/// the view touches or delegates.
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
                    content_elided: None,
                },
                wire: WireState::Elided,
            },
        ),
    ];
    for case in &cases {
        assert_eq!(
            serde_json::to_string(&EntryView::new(case)).unwrap(),
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
    let view = serde_json::to_string(&EntryView::new(&case)).unwrap();
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
    let view = serde_json::to_string(&EntryView::new(&case)).unwrap();
    let parsed: LogEntry = serde_json::from_str(&view).expect("view parses as LogEntry");
    let Payload::ToolResult { raw, .. } = &parsed.payload else {
        panic!("payload variant survived");
    };
    assert_eq!(raw.images.len(), 1);
    assert_eq!(raw.images[0].media_type, "image/png");
    assert_eq!(raw.images[0].data, "", "absent data reads back empty");
}

fn text_result(content: String) -> Payload {
    Payload::ToolResult {
        call_id: CallId("toolu_big".into()),
        raw: RawResult {
            content,
            is_error: false,
            images: Vec::new(),
            content_elided: None,
        },
        wire: WireState::Raw,
    }
}

/// The threshold's two sides. A small result is byte-identical through the windowed
/// view, the dereference view, and the derived path — the shape the frame ≡ window
/// pin depends on (frames.rs). An oversized one diverges exactly once: the windowed
/// view carries a prefix plus the tag, `EntryView::full` (what `GET /log/:seq`
/// serves) carries the whole string and no tag.
#[test]
fn oversized_content_is_elided_in_the_window_and_whole_at_the_entry_route() {
    let small = entry(7, None, text_result("x".repeat(MAX_INLINE_RESULT_BYTES)));
    let windowed = serde_json::to_string(&EntryView::new(&small)).unwrap();
    assert_eq!(
        windowed,
        serde_json::to_string(&EntryView::full(&small)).unwrap(),
        "at the threshold the two views are the same bytes"
    );
    assert_eq!(
        windowed,
        serde_json::to_string(&small).unwrap(),
        "and both are the derived bytes"
    );
    assert!(!windowed.contains("content_elided"), "no tag when whole");

    let content = "y".repeat(MAX_INLINE_RESULT_BYTES * 2);
    let big = entry(8, None, text_result(content.clone()));
    let windowed = serde_json::to_string(&EntryView::new(&big)).unwrap();
    let full = serde_json::to_string(&EntryView::full(&big)).unwrap();

    assert!(windowed.len() < full.len(), "the window carries less");
    assert_eq!(
        full,
        serde_json::to_string(&big).unwrap(),
        "the entry route serves the entry verbatim"
    );
    assert!(!full.contains("content_elided"), "and carries no tag");

    let parsed: LogEntry = serde_json::from_str(&windowed).expect("the view parses as a LogEntry");
    let Payload::ToolResult { raw, .. } = &parsed.payload else {
        panic!("payload variant survived");
    };
    assert_eq!(raw.content.len(), MAX_INLINE_RESULT_BYTES);
    assert!(content.starts_with(&raw.content), "a prefix, not a summary");
    assert_eq!(
        raw.content_elided,
        Some(ContentElision {
            kept_bytes: MAX_INLINE_RESULT_BYTES,
            total_bytes: MAX_INLINE_RESULT_BYTES * 2,
        }),
        "tagged with what was kept and what exists"
    );
}

/// The prefix is cut at a UTF-8 boundary, so `kept_bytes` can land below the
/// threshold rather than splitting a code point into invalid bytes.
#[test]
fn an_elided_prefix_ends_on_a_character_boundary() {
    // Three-byte characters do not divide the threshold evenly, so the cut lands mid
    // character unless it is walked back.
    let content = "☃".repeat(MAX_INLINE_RESULT_BYTES);
    let case = entry(9, None, text_result(content));
    let view = serde_json::to_string(&EntryView::new(&case)).unwrap();
    let parsed: LogEntry = serde_json::from_str(&view).expect("a valid string survived the cut");
    let Payload::ToolResult { raw, .. } = &parsed.payload else {
        panic!("payload variant survived");
    };
    assert!(raw.content.len() <= MAX_INLINE_RESULT_BYTES);
    assert!(raw.content.len() > MAX_INLINE_RESULT_BYTES - 3);
    assert_eq!(
        raw.content_elided.map(|e| e.kept_bytes),
        Some(raw.content.len()),
        "kept_bytes is the prefix actually carried"
    );
}
