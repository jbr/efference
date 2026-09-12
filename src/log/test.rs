use super::*;

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
            payload: Payload::Title(Title {
                text: "Second Title".to_string(),
                author: TitleAuthor::Triage,
            }),
        },
    ];
    assert_eq!(latest_title(&entries), Some("Second Title"));
}
