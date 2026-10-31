use super::*;
use crate::log::{AssistantOutput, ContentBlock, RawResult, ToolCall, WireState};
use crate::project::project;
use serde_json::json;

mod header;
mod open;
mod read;

/// Line 1 as a reader accepts it carrying no metadata — the header a fixture
/// persists. Built by hand rather than by a writer so a test can vary one
/// field of it and leave the rest alone; the format and version are the
/// reader's own, so a format bump leaves it current.
fn bare_header() -> String {
    format!(
        r#"{{"format":"{}","version":{},"metadata":null}}"#,
        LogHeader::FORMAT,
        LogHeader::VERSION
    )
}

/// One entry line at `seq`, holding the payload a `User` turn writes.
fn user_entry(seq: u64) -> String {
    format!(r#"{{"seq":{seq},"turn":0,"at":1,"payload":{{"User":[{{"Text":"hi"}}]}}}}"#)
}

/// A log path for one test, cleared of the file a previous run left there —
/// `create` is exclusive, so a leftover would fail the test for the wrong
/// reason.
fn temp_path(name: &str) -> std::path::PathBuf {
    let path = std::env::temp_dir().join(format!("efference-{name}-{}.jsonl", std::process::id()));
    let _ = std::fs::remove_file(&path);
    path
}

/// The file's lines from the second on: what an assertion about *entries*
/// means, since line 1 is the header (docs/log-and-projection.md).
fn entry_lines(path: &Path) -> Vec<String> {
    std::fs::read_to_string(path)
        .unwrap()
        .lines()
        .skip(1)
        .map(str::to_string)
        .collect()
}

/// Line 1 on its own, for an assertion that a header was left as it was.
fn header_line(path: &Path) -> String {
    std::fs::read_to_string(path)
        .unwrap()
        .lines()
        .next()
        .expect("a log has a header line")
        .to_string()
}

/// Why loading `path` failed, as the typed error a caller branches on — the
/// class, not the message.
fn load_error(path: &Path) -> LoadError {
    load(path).unwrap_err()
}

fn sample(log: &mut Log) {
    log.append(0, Payload::user("hello")).unwrap();
    log.append(
        1,
        Payload::assistant(AssistantOutput::from_legacy_messages(
            vec![],
            vec![ContentBlock::Text("let me check".into())],
            vec![ToolCall::new(
                "bash",
                "toolu_1",
                json!({ "command": "ls", "timeout": 5 }),
            )],
        )),
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
/// byte-stability guarantee as its oracle. The header line is on disk and
/// never in the projection, so it is in neither side of the comparison.
#[tokio::test]
async fn disk_roundtrip_projects_identically() {
    let path = temp_path("roundtrip");

    let mut log = Log::create(&path, None).unwrap();
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
/// `system` string (docs/triage.md), so a listed item's name / path /
/// description survive the log's serde and `project` can later fold them back into a
/// surfacing reminder. The flat `system` stays the byte-exact wire truth; this pins the
/// additive structured half.
#[test]
fn preamble_snapshots_the_structured_context_listing() {
    use crate::log::context::{Item, Resolved, Section};

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
        crate::log::Provider::OpenRouter,
        "google/gemini-3.7-flash",
    );
    let line = serde_json::to_string(&Payload::preamble(
        "You are m.",
        " sys",
        listing.clone(),
        Some(inference.clone()),
    ))
    .unwrap();

    match serde_json::from_str::<Payload>(&line).unwrap() {
        Payload::Preamble {
            identity,
            body,
            context,
            inference: recorded,
            ..
        } => {
            assert_eq!(format!("{identity}{body}"), "You are m. sys");
            assert_eq!(recorded.as_ref(), Some(&inference));
            assert_eq!(
                context, listing,
                "structured listing must survive the round-trip"
            );
        }
        other => panic!("expected a Preamble, got {other:?}"),
    }
}

/// An entry line predating the structured listing (bare `{"identity": …, "body": …}`) still
/// deserializes — the `#[serde(default)]` on `context` reads it back as an
/// empty `Resolved`, never a parse error. This is about an entry's shape; a
/// file with no header at all is refused for that reason instead (see `header`).
#[test]
fn legacy_preamble_without_context_reads_back_empty() {
    let legacy = r#"{"Preamble":{"identity":"You are m.","body":" an agent"}}"#;
    match serde_json::from_str::<Payload>(legacy).unwrap() {
        Payload::Preamble {
            identity,
            body,
            context,
            inference,
            ..
        } => {
            assert_eq!(format!("{identity}{body}"), "You are m. an agent");
            assert_eq!(context, crate::log::context::Resolved::default());
            assert!(inference.is_none());
        }
        other => panic!("expected a Preamble, got {other:?}"),
    }
}

/// The preamble's serialized shape, pinned: a resume on another main model rewrites
/// `identity` over an unchanged `body`, and the migration to this shape
/// (`scripts/migrate-log-v5.py`) writes these bytes.
#[test]
fn preamble_serializes_identity_then_body() {
    let line = serde_json::to_string(&Payload::preamble(
        "You are m.",
        " This session is `s`.",
        Default::default(),
        None,
    ))
    .unwrap();
    assert_eq!(
        line,
        r#"{"Preamble":{"identity":"You are m.","body":" This session is `s`.","context":{"sections":[]}}}"#
    );
}

/// `submission_id` (docs/control-api.md) survives the jsonl round-trip when
/// present and is skipped entirely when absent — so an entry whose writer predated
/// the field and one written now serialize byte-identically.
#[test]
fn submission_id_round_trips_and_is_skipped_when_absent() {
    let path = temp_path("submission");

    let mut log = Log::create(&path, None).unwrap();
    log.append_submission(0, Payload::user("hi"), Some("sub-abc".into()), None)
        .unwrap();
    log.append(0, Payload::user("note")).unwrap();
    drop(log);

    let lines = entry_lines(&path);
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
/// existed; an entry without it reads back as `None`.
#[test]
fn source_round_trips_and_is_skipped_when_absent() {
    let path = temp_path("source");

    let mut log = Log::create(&path, None).unwrap();
    log.append_submission(
        0,
        Payload::user("hi"),
        None,
        Some("mcp[claude-code]".into()),
    )
    .unwrap();
    log.append(0, Payload::user("note")).unwrap();
    drop(log);

    let lines = entry_lines(&path);
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
    with.append_submission(0, Payload::user("hello"), Some("sub-xyz".into()), None)
        .unwrap();
    let mut without = Log::in_memory();
    without.append(0, Payload::user("hello")).unwrap();
    assert_eq!(
        serde_json::to_string(&project(with.entries())).unwrap(),
        serde_json::to_string(&project(without.entries())).unwrap(),
    );
}

/// The fork's recorded reasoning (`Triage.thinking`, docs/triage.md) survives the
/// jsonl round-trip, one entry per triage attempt that thought.
#[test]
fn triage_thinking_round_trips() {
    let thinking = vec![
        "attempt 1: the ask wants a verdict, not a work tool".into(),
        "attempt 2: keep the failing assertion, drop the build noise".into(),
    ];
    let line = serde_json::to_string(&Payload::Triage(
        crate::log::Triage::new(Some(7), "40-52").with_thinking(thinking.clone()),
    ))
    .unwrap();

    match serde_json::from_str::<Payload>(&line).unwrap() {
        Payload::Triage(t) => assert_eq!(t.thinking, thinking),
        other => panic!("expected a Triage, got {other:?}"),
    }
}

/// A `Triage` entry predating the field still deserializes — `#[serde(default)]`
/// reads it back as no recorded reasoning, never a parse error. As above, this is
/// about an entry's shape, not about a file lacking its header.
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
