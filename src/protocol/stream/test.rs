use super::*;

/// Every variant of the enum, once.
fn every_delta() -> Vec<StreamDelta> {
    vec![
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
            fragment: "{".into(),
        },
        StreamDelta::ToolOutput {
            call_id: "toolu_1".into(),
            chunk: "o".into(),
        },
        StreamDelta::ProcessOutput {
            id: 1,
            chunk: "o".into(),
        },
        StreamDelta::CompactionText { text: "c".into() },
        StreamDelta::CompactionThinking { text: "c".into() },
        StreamDelta::ConsolidationText { text: "c".into() },
        StreamDelta::ConsolidationThinking { text: "c".into() },
        StreamDelta::TriageText {
            call_ids: vec!["toolu_1".into()],
            text: "x".into(),
        },
        StreamDelta::TriageThinking {
            call_ids: Vec::new(),
            text: "x".into(),
        },
    ]
}

/// `kind` and the serialized `kind` are one name written twice, so they are compared:
/// the frame envelope names the first, and the schema and the data come from the second.
#[test]
fn a_delta_names_the_kind_it_serializes_under() {
    for delta in every_delta() {
        let serialized = serde_json::to_value(&delta).unwrap();
        assert_eq!(serialized["kind"], delta.kind());
        assert!(serialized["data"].is_object());
    }
}

#[test]
fn every_variant_has_its_own_kind() {
    let kinds: std::collections::BTreeSet<&str> =
        every_delta().iter().map(StreamDelta::kind).collect();
    assert_eq!(kinds.len(), every_delta().len());
}
