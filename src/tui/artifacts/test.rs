use super::*;
use crate::log::{AssistantOutput, ContentBlock, Payload, Timestamp};

#[test]
fn candidate_coordinates_follow_visible_text_blocks_and_all_fences() {
    let output = AssistantOutput::from_legacy_messages(
        vec![],
        vec![
            ContentBlock::Text("plain response".into()),
            ContentBlock::Text(
                concat!(
                    "```rust\nlet value = 1;\n```\n\n",
                    "~~~html-artifact\n<svg/>\n~~~\n\n",
                    "```html-artifact preview\n<svg/>\n```\n\n",
                    "```html-artifact\nunclosed\n",
                )
                .into(),
            ),
        ],
        vec![],
    );
    let entry = LogEntry {
        seq: 19,
        turn: 1,
        at: Timestamp(0),
        submission_id: None,
        source: None,
        payload: Payload::Assistant {
            output,
            excluded: None,
        },
    };

    assert_eq!(
        candidates(&entry),
        [Coordinate {
            seq: 19,
            text_block: 1,
            fence: 1,
        }]
    );
}

#[test]
fn user_text_never_produces_artifact_coordinates() {
    let entry = LogEntry {
        seq: 20,
        turn: 1,
        at: Timestamp(0),
        submission_id: None,
        source: None,
        payload: Payload::User(vec![ContentBlock::Text(
            "```html-artifact\n<svg/>\n```".into(),
        )]),
    };

    assert!(candidates(&entry).is_empty());
}

#[test]
fn excluded_assistant_text_never_produces_artifact_coordinates() {
    let entry = LogEntry {
        seq: 21,
        turn: 1,
        at: Timestamp(0),
        submission_id: None,
        source: None,
        payload: Payload::Assistant {
            output: AssistantOutput::from_legacy_messages(
                vec![],
                vec![ContentBlock::Text("```html-artifact\n<svg/>\n```".into())],
                vec![],
            ),
            excluded: Some(crate::log::Exclusion::Empty),
        },
    };

    assert!(candidates(&entry).is_empty());
}
