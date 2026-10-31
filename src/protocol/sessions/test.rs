use super::*;

/// A listing's status word is spelled one way: serde's is `as_str`'s, which is also what
/// a client prints.
#[test]
fn a_listing_status_is_spelled_one_way() {
    for (status, word) in [
        (ListedStatus::Starting, "starting"),
        (ListedStatus::Running, "running"),
        (ListedStatus::Idle, "idle"),
        (ListedStatus::Gated, "gated"),
        (ListedStatus::Asking, "asking"),
        (ListedStatus::Stalled, "stalled"),
        (ListedStatus::Parked, "parked"),
        (ListedStatus::Blocked, "blocked"),
    ] {
        assert_eq!(
            serde_json::to_value(&status).unwrap(),
            serde_json::Value::from(word)
        );
        assert_eq!(status.as_str(), word);
        assert_eq!(status.to_string(), word);
        assert_eq!(
            serde_json::from_value::<ListedStatus>(word.into()).unwrap(),
            status
        );
    }
}

/// A word this build does not name still decodes, as the word it came as: a client reads
/// the listing of a host newer than itself, shows the word, and writes it back unchanged.
/// A value that is not a word is refused, as it is for any string.
#[test]
fn an_unknown_listing_status_decodes_as_the_word_it_came_as() {
    for word in ["hibernating", "Running", ""] {
        let text = format!(
            r#"{{"id":"a","status":{}}}"#,
            serde_json::to_string(word).unwrap()
        );
        let row: ListedSession = serde_json::from_str(&text).unwrap();
        assert_eq!(row.status, ListedStatus::Other(word.into()));
        assert_eq!(row.status.as_str(), word);
        assert_eq!(row.status.to_string(), word);
        assert_eq!(
            serde_json::to_string(&row).unwrap(),
            format!(
                r#"{{"id":"a","status":{},"processes":0}}"#,
                serde_json::to_string(word).unwrap()
            ),
        );
    }
    for value in ["7", "null", r#"{"running":null}"#, r#"["running"]"#] {
        let text = format!(r#"{{"id":"a","status":{value}}}"#);
        assert!(
            serde_json::from_str::<ListedSession>(&text).is_err(),
            "{text}"
        );
    }
}

/// A row is serialized in the order its fields are declared and leaves out what it has
/// not got.
#[test]
fn a_listed_session_keeps_its_bytes() {
    let row = ListedSession {
        title: Some("Fix the parser".into()),
        cwd: Some("/work".into()),
        working: Some("/work/.efference/worktrees/later".into()),
        created_by: Some("early".into()),
        processes: 2,
        ..ListedSession::new("later", ListedStatus::Parked)
    };
    assert_eq!(
        serde_json::to_string(&row).unwrap(),
        r#"{"id":"later","title":"Fix the parser","cwd":"/work","working":"/work/.efference/worktrees/later","created_by":"early","status":"parked","processes":2}"#
    );
    let bare: ListedSession = serde_json::from_str(r#"{"id":"a","status":"idle"}"#).unwrap();
    assert_eq!(bare.processes, 0, "a row without a count holds none");
}

/// An archive's answer leaves out what does not apply: `resumed` on a `DELETE`, a
/// worktree the session did not make, and the sessions that did not end with it.
#[test]
fn an_archive_answer_leaves_out_what_does_not_apply() {
    let answer = SessionArchived {
        id: "a".into(),
        state: ArchiveOutcome::Discarded,
        resumed: None,
        worktree: None,
        ended_with: Vec::new(),
    };
    assert_eq!(
        serde_json::to_string(&answer).unwrap(),
        r#"{"id":"a","state":"discarded"}"#
    );
    let answer = SessionArchived {
        resumed: Some(false),
        worktree: Some(RetiredWorktree::Kept { kept: "k".into() }),
        ended_with: vec!["b".into()],
        ..answer
    };
    assert_eq!(
        serde_json::to_string(&answer).unwrap(),
        r#"{"id":"a","state":"discarded","resumed":false,"worktree":{"kept":"k"},"ended_with":["b"]}"#
    );
}
