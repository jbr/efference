//! The bytes each `output` frame serializes to (docs/control-api.md), one pin per kind.
//! The clients parse these, so a change to them is a change to the control channel and
//! fails here first, as the state topics' bytes do (`topic_bytes.rs`).
//!
//! The comparison is of whole strings, key order included: the frame's own keys
//! (`class`, `kind`, `data`) in the order the envelope declares them, and the keys of
//! `data` sorted, as every object the channel builds from a `serde_json::Value` is.
//! Each input carries a character JSON escapes, so the escaping is pinned with the
//! keys.

use super::*;
use crate::protocol::stream::StreamDelta;

fn frame(delta: &StreamDelta) -> String {
    Frame::output(delta).to_json()
}

#[test]
fn the_model_stream_frames_keep_their_bytes() {
    assert_eq!(
        frame(&StreamDelta::AssistantText {
            index: 0,
            text: "say \"hi\"\n".into(),
        }),
        r#"{"class":"output","kind":"assistant_delta","data":{"index":0,"text":"say \"hi\"\n"}}"#
    );
    assert_eq!(
        frame(&StreamDelta::ThinkingText {
            index: 2,
            text: "hmm — é".into(),
        }),
        r#"{"class":"output","kind":"thinking_delta","data":{"index":2,"text":"hmm — é"}}"#
    );
    assert_eq!(
        frame(&StreamDelta::ToolCallStart {
            index: 1,
            call_id: "toolu_1".into(),
            name: "bash".into(),
        }),
        r#"{"class":"output","kind":"tool_call_start","data":{"call_id":"toolu_1","index":1,"name":"bash"}}"#
    );
    assert_eq!(
        frame(&StreamDelta::ToolCallInput {
            index: 1,
            fragment: "{\"command\":\"ls".into(),
        }),
        r#"{"class":"output","kind":"tool_call_input","data":{"fragment":"{\"command\":\"ls","index":1}}"#
    );
}

#[test]
fn the_tool_and_process_frames_keep_their_bytes() {
    assert_eq!(
        frame(&StreamDelta::ToolOutput {
            call_id: "toolu_1".into(),
            chunk: "a\tb\n".into(),
        }),
        r#"{"class":"output","kind":"tool_output","data":{"call_id":"toolu_1","chunk":"a\tb\n"}}"#
    );
    assert_eq!(
        frame(&StreamDelta::ProcessOutput {
            id: 3,
            chunk: "ready\n".into(),
        }),
        r#"{"class":"output","kind":"process_output","data":{"chunk":"ready\n","id":3}}"#
    );
}

#[test]
fn the_side_request_frames_keep_their_bytes() {
    assert_eq!(
        frame(&StreamDelta::CompactionText {
            text: "the summary".into(),
        }),
        r#"{"class":"output","kind":"compaction_delta","data":{"text":"the summary"}}"#
    );
    assert_eq!(
        frame(&StreamDelta::CompactionThinking {
            text: "weighing".into(),
        }),
        r#"{"class":"output","kind":"compaction_thinking_delta","data":{"text":"weighing"}}"#
    );
    assert_eq!(
        frame(&StreamDelta::ConsolidationText {
            text: "the account".into(),
        }),
        r#"{"class":"output","kind":"consolidation_delta","data":{"text":"the account"}}"#
    );
    assert_eq!(
        frame(&StreamDelta::ConsolidationThinking {
            text: "sorting".into(),
        }),
        r#"{"class":"output","kind":"consolidation_thinking_delta","data":{"text":"sorting"}}"#
    );
    assert_eq!(
        frame(&StreamDelta::TriageText {
            call_ids: vec!["toolu_1".into(), "toolu_2".into()],
            text: "keep 1-3".into(),
        }),
        r#"{"class":"output","kind":"triage_delta","data":{"call_ids":["toolu_1","toolu_2"],"text":"keep 1-3"}}"#
    );
    assert_eq!(
        frame(&StreamDelta::TriageThinking {
            call_ids: Vec::new(),
            text: "idle".into(),
        }),
        r#"{"class":"output","kind":"triage_thinking_delta","data":{"call_ids":[],"text":"idle"}}"#
    );
}
