use super::*;

use crate::log::{Payload, Seq, Timestamp};

/// The shape older logs wrote — `"upstream_failure": "<prose>"` — still parses.
/// The log is append-only and never rewritten, so an entry from before
/// diagnostics existed has to keep reading back or the session's history is
/// unloadable.
#[test]
fn a_bare_string_still_deserializes() {
    let record: UpstreamFailureRecord =
        serde_json::from_str(r#""stream finalized with no content and no usage""#).unwrap();
    assert_eq!(
        record.why(),
        "stream finalized with no content and no usage"
    );
    assert!(record.stream.is_none());
}

/// And a record with no diagnostics writes that same shape back, so the ordinary
/// failure paths (a mid-stream death, an absorbed status) produce byte-identical
/// jsonl to what they produced before this type existed.
#[test]
fn no_diagnostics_serializes_as_a_bare_string() {
    let record = UpstreamFailureRecord::from("upstream attempt failed (HTTP 429): slow down");
    assert_eq!(
        serde_json::to_string(&record).unwrap(),
        r#""upstream attempt failed (HTTP 429): slow down""#
    );
}

/// With diagnostics it is an object, and every captured field round-trips —
/// this is the whole point of the entry: the next investigation reads it back
/// out of the jsonl months later.
#[test]
fn diagnostics_round_trip() {
    let record = UpstreamFailureRecord {
        why: "stream finalized with no content and no usage".to_string(),
        stream: Some(StreamDiagnostics {
            status: 200,
            headers: BTreeMap::from([("x-generation-id".to_string(), "gen-abc".to_string())]),
            message_start: Some(r#"{"message":{"id":"gen-abc"}}"#.to_string()),
            frames: "event: message_start\ndata: {}\n\n".to_string(),
            frames_truncated: false,
        }),
    };
    let line = serde_json::to_string(&record).unwrap();
    assert!(line.starts_with(r#"{"why":"stream finalized"#), "{line}");
    let read_back: UpstreamFailureRecord = serde_json::from_str(&line).unwrap();
    assert_eq!(read_back, record);
}

/// The whole payload round-trips through the log's own serialization, from
/// either spelling of the record — the entry, not just the field, is what a session
/// reloads.
#[test]
fn an_assistant_entry_round_trips_from_either_spelling() {
    for json in [
        r#"{"seq":7,"turn":2,"at":1,"payload":{"Assistant":{"output":{"Messages":[]},"excluded":{"Upstream":"stream aborted"}}}}"#,
        r#"{"seq":7,"turn":2,"at":1,"payload":{"Assistant":{"output":{"Messages":[]},"excluded":{"Upstream":{"why":"stream aborted","stream":{"status":200}}}}}}"#,
    ] {
        let entry: crate::log::LogEntry = serde_json::from_str(json).unwrap();
        let Payload::Assistant {
            excluded: Some(crate::log::Exclusion::Upstream(failure)),
            ..
        } = &entry.payload
        else {
            panic!("expected a record-only assistant entry")
        };
        assert_eq!(failure.why(), "stream aborted");
        assert_eq!(entry.seq, 7 as Seq);
        assert_eq!(entry.at, Timestamp(1));
        assert_eq!(serde_json::to_string(&entry).unwrap(), json);
    }
}

/// An empty completion's mark is the bare variant name (docs/log-and-projection.md).
#[test]
fn an_empty_completion_serializes_as_its_kind() {
    let json = r#"{"seq":7,"turn":2,"at":1,"payload":{"Assistant":{"output":{"Messages":[]},"excluded":"Empty"}}}"#;
    let entry: crate::log::LogEntry = serde_json::from_str(json).unwrap();
    assert!(matches!(
        entry.payload,
        Payload::Assistant {
            excluded: Some(crate::log::Exclusion::Empty),
            ..
        }
    ));
    assert_eq!(serde_json::to_string(&entry).unwrap(), json);
}

/// The allowlist decides what may reach the log; a header outside it is never
/// even offered to the masker (docs/env.md).
#[test]
fn the_header_allowlist_is_case_insensitive_and_closed() {
    assert!(is_captured_header("X-Generation-Id"));
    assert!(is_captured_header("CONTENT-TYPE"));
    assert!(!is_captured_header("authorization"));
    assert!(!is_captured_header("set-cookie"));
    assert!(!is_captured_header("x-openrouter-account"));
}
