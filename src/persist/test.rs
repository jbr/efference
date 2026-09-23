use super::*;
use crate::log::{AssistantOutput, ContentBlock, RawResult, ToolCall, WireState};
use crate::project::project;
use serde_json::json;

fn sample(log: &mut Log) {
    log.append(0, Payload::User(vec![ContentBlock::Text("hello".into())]))
        .unwrap();
    log.append(
        1,
        Payload::Assistant {
            output: AssistantOutput::from_legacy_messages(
                vec![],
                vec![ContentBlock::Text("let me check".into())],
                vec![ToolCall::new(
                    "bash",
                    "toolu_1",
                    json!({ "command": "ls", "timeout": 5 }),
                )],
            ),
            upstream_failure: None,
        },
    )
    .unwrap();
    log.append(
        1,
        Payload::tool_result("toolu_1", RawResult::text("file.txt"), WireState::Raw),
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
    let inference = crate::log::LoggedInference::new(
        "recorded",
        crate::api::Provider::OpenRouter,
        "google/gemini-3.7-flash",
    );
    let line = serde_json::to_string(&Payload::Preamble {
        system: "sys".into(),
        context: listing.clone(),
        inference: Some(inference.clone()),
    })
    .unwrap();

    match serde_json::from_str::<Payload>(&line).unwrap() {
        Payload::Preamble {
            system,
            context,
            inference: recorded,
        } => {
            assert_eq!(system, "sys");
            assert_eq!(recorded.as_ref(), Some(&inference));
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
        Payload::Preamble {
            system,
            context,
            inference,
        } => {
            assert_eq!(system, "you are an agent");
            assert_eq!(context, crate::context::Resolved::default());
            assert!(inference.is_none());
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
        None,
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

/// `source` rides on the entry like `submission_id`, but unlike it is
/// projected (docs/control-api.md). Written only when a client names itself, so
/// an operator turn's jsonl is byte-identical to one written before the field
/// existed; old logs without it load as `None`.
#[test]
fn source_round_trips_and_is_skipped_when_absent() {
    let path = std::env::temp_dir().join(format!("efference-source-{}.jsonl", std::process::id()));
    let _ = std::fs::remove_file(&path);

    let mut log = Log::create(&path).unwrap();
    log.append_submission(
        0,
        Payload::User(vec![ContentBlock::Text("hi".into())]),
        None,
        Some("mcp[claude-code]".into()),
    )
    .unwrap();
    log.append(0, Payload::User(vec![ContentBlock::Text("note".into())]))
        .unwrap();
    drop(log);

    let raw = std::fs::read_to_string(&path).unwrap();
    let lines: Vec<&str> = raw.lines().collect();
    assert!(lines[0].contains(r#""source":"mcp[claude-code]""#));
    assert!(!lines[1].contains("source"));

    let loaded = load(&path).unwrap();
    assert_eq!(loaded[0].source.as_deref(), Some("mcp[claude-code]"));
    assert_eq!(loaded[1].source, None);

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
        None,
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

/// The fork's recorded reasoning (`Triage.thinking`, docs/redaction-fork.md) survives the
/// jsonl round-trip, one entry per peek attempt that thought.
#[test]
fn triage_thinking_round_trips() {
    let thinking = vec![
        "attempt 1: the ask wants a verdict, not a work tool".into(),
        "attempt 2: keep the failing assertion, drop the build noise".into(),
    ];
    let line = serde_json::to_string(&Payload::Triage(crate::log::Triage {
        for_result: Some(7),
        origin: crate::log::Origin::Model,
        raw_keep: "40-52".into(),
        inbox: Vec::new(),
        surfaced: Vec::new(),
        retries: Vec::new(),
        thinking: thinking.clone(),
    }))
    .unwrap();

    match serde_json::from_str::<Payload>(&line).unwrap() {
        Payload::Triage(t) => assert_eq!(t.thinking, thinking),
        other => panic!("expected a Triage, got {other:?}"),
    }
}

/// A `Triage` line predating the field still loads — `#[serde(default)]` reads it back as
/// no recorded reasoning, never a parse error. Old logs must remain replayable.
#[test]
fn legacy_triage_without_thinking_reads_back_empty() {
    let legacy = r#"{"Triage":{"for_result":7,"origin":"Model","raw_keep":"all","inbox":[]}}"#;
    match serde_json::from_str::<Payload>(legacy).unwrap() {
        Payload::Triage(t) => {
            assert_eq!(t.raw_keep, "all");
            assert!(t.thinking.is_empty());
            assert!(t.retries.is_empty());
        }
        other => panic!("expected a Triage, got {other:?}"),
    }
}
