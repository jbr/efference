use super::*;
use crate::log::{CallId, ContentBlock, RawResult, ToolCall, WireState};
use crate::project::project;
use serde_json::json;

fn sample(log: &mut Log) {
    log.append(0, Payload::User(vec![ContentBlock::Text("hello".into())]))
        .unwrap();
    log.append(
        1,
        Payload::Assistant {
            thinking: vec![],
            text: vec![ContentBlock::Text("let me check".into())],
            calls: vec![ToolCall {
                id: CallId("toolu_1".into()),
                name: "bash".into(),
                input: json!({ "command": "ls", "timeout": 5 }),
            }],
            upstream_failure: None,
        },
    )
    .unwrap();
    log.append(
        1,
        Payload::ToolResult {
            call_id: CallId("toolu_1".into()),
            raw: RawResult {
                content: "file.txt".into(),
                is_error: false,
            },
            wire: WireState::Raw,
        },
    )
    .unwrap();
}

/// The headline property: a log written to disk and read back projects to the
/// exact same wire bytes — persistence is faithful, and reuses the existing
/// byte-stability guarantee as its oracle.
#[tokio::test]
async fn disk_roundtrip_projects_identically() {
    let path =
        std::env::temp_dir().join(format!("efference-roundtrip-{}.jsonl", std::process::id()));
    let _ = std::fs::remove_file(&path);

    let mut log = Log::create(&path).unwrap();
    sample(&mut log);
    let expected = serde_json::to_string(&project(log.entries())).unwrap();
    drop(log); // flush + close.

    let loaded = load(&path).unwrap();
    let actual = serde_json::to_string(&project(&loaded)).unwrap();
    assert_eq!(actual, expected, "disk round-trip must project identically");
    assert_eq!(loaded.len(), 3);

    std::fs::remove_file(&path).unwrap();
}

/// The boot preamble snapshots the **structured** `[context]` listing beside the flat
/// `system` string (docs/redaction-fork.md), so a listed item's name / path /
/// description survive the log's serde and `project` can later fold them back into a
/// surfacing reminder. The flat `system` stays the byte-exact wire truth; this pins the
/// additive structured half.
#[test]
fn preamble_snapshots_the_structured_context_listing() {
    use crate::context::{Item, Resolved, Section};

    let listing = Resolved {
        sections: vec![Section {
            heading: Some("Skills".into()),
            description: None,
            items: vec![Item::Listed {
                name: "ferritin".into(),
                description: "look up Rust docs".into(),
                path: "skills/ferritin.md".into(),
            }],
        }],
    };
    let line = serde_json::to_string(&Payload::Preamble {
        system: "sys".into(),
        context: listing.clone(),
    })
    .unwrap();

    match serde_json::from_str::<Payload>(&line).unwrap() {
        Payload::Preamble { system, context } => {
            assert_eq!(system, "sys");
            assert_eq!(
                context, listing,
                "structured listing must survive the round-trip"
            );
        }
        other => panic!("expected a Preamble, got {other:?}"),
    }
}

/// A log line predating the structured listing (bare `{"system": …}`) still loads — the
/// `#[serde(default)]` on `context` reads it back as an empty `Resolved`, never a parse
/// error. Old logs must remain replayable.
#[test]
fn legacy_preamble_without_context_reads_back_empty() {
    let legacy = r#"{"Preamble":{"system":"you are an agent"}}"#;
    match serde_json::from_str::<Payload>(legacy).unwrap() {
        Payload::Preamble { system, context } => {
            assert_eq!(system, "you are an agent");
            assert_eq!(context, crate::context::Resolved::default());
        }
        other => panic!("expected a Preamble, got {other:?}"),
    }
}

/// `submission_id` (docs/control-api.md) survives the jsonl round-trip when
/// present and is skipped entirely when absent — so a log predating the field
/// and an entry the loop originated itself serialize byte-identically to
/// before it existed.
#[test]
fn submission_id_round_trips_and_is_skipped_when_absent() {
    let path =
        std::env::temp_dir().join(format!("efference-submission-{}.jsonl", std::process::id()));
    let _ = std::fs::remove_file(&path);

    let mut log = Log::create(&path).unwrap();
    log.append_submission(
        0,
        Payload::User(vec![ContentBlock::Text("hi".into())]),
        Some("sub-abc".into()),
    )
    .unwrap();
    log.append(0, Payload::User(vec![ContentBlock::Text("note".into())]))
        .unwrap();
    drop(log);

    let raw = std::fs::read_to_string(&path).unwrap();
    let lines: Vec<&str> = raw.lines().collect();
    assert!(lines[0].contains(r#""submission_id":"sub-abc""#));
    assert!(!lines[1].contains("submission_id"));

    let loaded = load(&path).unwrap();
    assert_eq!(loaded[0].submission_id.as_deref(), Some("sub-abc"));
    assert_eq!(loaded[1].submission_id, None);

    std::fs::remove_file(&path).unwrap();
}

/// `submission_id` is log metadata like `at` (docs/log-and-projection.md):
/// two logs identical except for it must project to the same wire bytes.
#[test]
fn submission_id_never_reaches_the_projection() {
    let mut with = Log::in_memory();
    with.append_submission(
        0,
        Payload::User(vec![ContentBlock::Text("hello".into())]),
        Some("sub-xyz".into()),
    )
    .unwrap();
    let mut without = Log::in_memory();
    without
        .append(0, Payload::User(vec![ContentBlock::Text("hello".into())]))
        .unwrap();
    assert_eq!(
        serde_json::to_string(&project(with.entries())).unwrap(),
        serde_json::to_string(&project(without.entries())).unwrap(),
    );
}

/// Blank lines tolerated; entry count survives.
#[test]
fn load_tolerates_blank_lines() {
    let path = std::env::temp_dir().join(format!("efference-blank-{}.jsonl", std::process::id()));
    std::fs::write(
        &path,
        "{\"seq\":0,\"turn\":0,\"at\":1,\"payload\":{\"User\":[{\"Text\":\"hi\"}]}}\n\n",
    )
    .unwrap();
    let loaded = load(&path).unwrap();
    assert_eq!(loaded.len(), 1);
    std::fs::remove_file(&path).unwrap();
}
