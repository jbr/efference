use super::*;

/// `source` is optional on the entry (docs/control-api.md): an unsourced entry
/// serializes without it and parses back to `None`, which is what lets every
/// log written before the field existed load unchanged.
#[test]
fn log_entry_source_is_optional_and_round_trips() {
    let unsourced = LogEntry {
        seq: 0,
        turn: 0,
        at: Timestamp::now(),
        submission_id: None,
        source: None,
        payload: Payload::User(vec![ContentBlock::Text("hi".into())]),
    };
    let json = serde_json::to_string(&unsourced).unwrap();
    assert!(!json.contains("source"), "{json}");
    let back: LogEntry = serde_json::from_str(&json).unwrap();
    assert_eq!(back.source, None);

    let sourced = LogEntry {
        source: Some("web".into()),
        ..unsourced
    };
    let json = serde_json::to_string(&sourced).unwrap();
    assert!(json.contains(r#""source":"web""#), "{json}");
    let back: LogEntry = serde_json::from_str(&json).unwrap();
    assert_eq!(back.source.as_deref(), Some("web"));
}

#[test]
fn latest_title_empty_log_returns_none() {
    let entries: Vec<LogEntry> = vec![];
    assert!(latest_title(&entries).is_none());
}

#[test]
fn latest_title_latest_entry_wins() {
    let entries = vec![
        LogEntry {
            seq: 1,
            turn: 0,
            at: Timestamp::now(),
            submission_id: None,
            source: None,
            payload: Payload::Title(Title {
                text: "First Title".to_string(),
                author: TitleAuthor::Triage,
            }),
        },
        LogEntry {
            seq: 2,
            turn: 0,
            at: Timestamp::now(),
            submission_id: None,
            source: None,
            payload: Payload::Title(Title {
                text: "Second Title".to_string(),
                author: TitleAuthor::Triage,
            }),
        },
    ];
    assert_eq!(latest_title(&entries), Some("Second Title"));
}
