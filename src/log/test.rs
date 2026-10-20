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
fn a_worktree_entry_round_trips() {
    let kinds = [
        WorktreeEventKind::Added(Checkout {
            base: "abc123".into(),
            exit_code: Some(1),
            output: "hook failed\n".into(),
        }),
        WorktreeEventKind::Restored(Checkout {
            base: "abc123".into(),
            exit_code: Some(0),
            output: String::new(),
        }),
        WorktreeEventKind::Removed {
            branch_deleted: true,
        },
        WorktreeEventKind::Kept {
            reason: "uncommitted changes: ?? a.txt".into(),
        },
        WorktreeEventKind::RemoveFailed {
            error: "git failed".into(),
        },
    ];
    for kind in kinds {
        let entry = LogEntry {
            seq: 1,
            turn: 0,
            at: Timestamp::now(),
            submission_id: None,
            source: None,
            payload: Payload::Worktree(WorktreeEvent {
                path: "/p/.efference/worktrees/quiet-fox".into(),
                branch: "efference/quiet-fox".into(),
                kind,
            }),
        };
        let json = serde_json::to_string(&entry).unwrap();
        let back: LogEntry = serde_json::from_str(&json).unwrap();
        assert!(
            matches!(
                (&back.payload, &entry.payload),
                (Payload::Worktree(a), Payload::Worktree(b)) if a == b
            ),
            "{json}"
        );
    }
}

#[test]
fn recorded_inference_comes_from_the_latest_boot_preamble() {
    let entry = |seq, inference: Option<LoggedInference>| LogEntry {
        seq,
        turn: 0,
        at: Timestamp::now(),
        submission_id: None,
        source: None,
        payload: Payload::preamble("", "sys", Default::default(), inference),
    };
    let first = LoggedInference::new("first", crate::api::Provider::Anthropic, "claude");
    let second = LoggedInference::new("second", crate::api::Provider::OpenRouter, "google/gemini");
    let entries = vec![
        entry(0, Some(first.clone())),
        entry(1, Some(second.clone())),
    ];
    assert_eq!(recorded_inference(&entries), Some(&second));

    // A later boot can predate identity metadata. Its missing value is authoritative;
    // the older identity must not be resurrected.
    let latest_without_identity = vec![entry(0, Some(first)), entry(1, None)];
    assert_eq!(recorded_inference(&latest_without_identity), None);

    let legacy = vec![entry(0, None)];
    assert_eq!(recorded_inference(&legacy), None);
}

#[test]
fn logged_inference_has_a_stable_json_shape() {
    let identity = LoggedInference::new(
        "router",
        crate::api::Provider::OpenRouter,
        "google/gemini-3.7-flash",
    );
    let json = serde_json::to_string(&identity).unwrap();
    assert_eq!(
        json,
        r#"{"connection":"router","provider":"openrouter","model":"google/gemini-3.7-flash"}"#
    );
    let back: LoggedInference = serde_json::from_str(&json).unwrap();
    assert_eq!(back, identity);
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

/// A decision-rung verdict carries its whole answer vector, and an entry written
/// before the field existed still reads (docs/permissions.md).
#[test]
fn a_spot_carries_the_decision_axes_and_old_entries_still_read() {
    let axes = crate::spotter::jev::Axes {
        in_scope: 0.94,
        path_outside: 0.19,
        sketch: Some(vec![0.9, 0.05, 0.03, 0.01, 0.01]),
        ..Default::default()
    };
    let spot = Spot {
        call_id: CallId("c1".into()),
        stance: Stance::Clear,
        reason: "routine (level 0).".into(),
        origin: Origin::Model,
        model: Some("jev-1.13.0".into()),
        probed_prefix: None,
        axes: Some(axes.clone()),
    };
    let json = serde_json::to_string(&spot).unwrap();
    assert!(json.contains(r#""axes":{"in_scope":0.94,"#), "{json}");
    let back: Spot = serde_json::from_str(&json).unwrap();
    assert_eq!(back.axes, Some(axes));

    let old = r#"{"call_id":"c1","stance":"clear","reason":"r","origin":"Model"}"#;
    let back: Spot = serde_json::from_str(old).unwrap();
    assert_eq!(back.axes, None);
}

/// Telemetry nests under one key, `{"Telemetry": {"<Variant>": …}}` — the shape
/// version 2 of the log format introduced (docs/log-and-projection.md). Changing it
/// is a log-format change: every recorded log would stop reading.
#[test]
fn telemetry_nests_under_one_key() {
    let payload = Payload::Telemetry(Telemetry::CallStarted {
        call_id: "toolu_a".into(),
    });
    let json = serde_json::to_string(&payload).unwrap();
    assert_eq!(
        json,
        r#"{"Telemetry":{"CallStarted":{"call_id":"toolu_a"}}}"#
    );
    let back: Payload = serde_json::from_str(&json).unwrap();
    assert!(matches!(
        back,
        Payload::Telemetry(Telemetry::CallStarted { call_id }) if call_id == CallId::from("toolu_a")
    ));
}

/// `scripts/migrate-log-archived.py` appends this exact line to an existing log, so the
/// serialized form of an `Archived` entry is pinned here.
#[test]
fn an_archived_entry_has_a_stable_json_shape() {
    let entry = LogEntry {
        seq: 8,
        turn: 2,
        at: Timestamp(1_782_260_379_694),
        submission_id: None,
        source: None,
        payload: Payload::Archived { source: None },
    };
    assert_eq!(
        serde_json::to_string(&entry).unwrap(),
        r#"{"seq":8,"turn":2,"at":1782260379694,"payload":{"Archived":{}}}"#
    );
}

/// An archive that records who made it writes `source` inside the payload, and reads it
/// back; the entries written before the field existed read with none.
#[test]
fn an_archived_entry_round_trips_its_source() {
    let entry = LogEntry {
        seq: 8,
        turn: 2,
        at: Timestamp(1_782_260_379_694),
        submission_id: None,
        source: None,
        payload: Payload::archived(Some("mcp[claude-code]".into())),
    };
    let line = serde_json::to_string(&entry).unwrap();
    assert_eq!(
        line,
        r#"{"seq":8,"turn":2,"at":1782260379694,"payload":{"Archived":{"source":"mcp[claude-code]"}}}"#
    );
    let back: LogEntry = serde_json::from_str(&line).unwrap();
    assert!(matches!(
        back.payload,
        Payload::Archived { source: Some(ref source) } if source == "mcp[claude-code]"
    ));

    let old: LogEntry =
        serde_json::from_str(r#"{"seq":8,"turn":2,"at":1782260379694,"payload":{"Archived":{}}}"#)
            .unwrap();
    assert!(matches!(old.payload, Payload::Archived { source: None }));
}

/// A `tools[]` difference is named by tool: added, removed, changed, or only reordered.
#[test]
fn tools_change_names_what_moved() {
    use super::ToolsChange;
    let tool = |name: &str, d: &str| serde_json::json!({"name": name, "description": d});
    let before = vec![tool("a", "x"), tool("b", "x"), tool("c", "x")];
    let now = vec![tool("a", "x"), tool("b", "y"), tool("d", "x")];
    let change = ToolsChange::between(&before, &now);
    assert_eq!(change.added, vec!["d"]);
    assert_eq!(change.removed, vec!["c"]);
    assert_eq!(change.changed, vec!["b"]);
    assert!(!change.reordered);
    assert!(ToolsChange::between(&before, &before).is_empty());
    let mut swapped = before.clone();
    swapped.swap(0, 1);
    let change = ToolsChange::between(&before, &swapped);
    assert!(change.reordered && change.added.is_empty());
}

/// The push event written when a held push goes out is `released` (log version 7,
/// docs/web-push.md); `scripts/migrate-log-v7.py` rewrites the earlier spelling.
#[test]
fn a_released_push_serializes_as_released() {
    let record = PushRecord {
        moment: 4,
        kind: MomentKind::Settle,
        event: PushEvent::Released,
    };
    let json = serde_json::to_string(&Telemetry::Push(record.clone())).unwrap();
    assert_eq!(
        json,
        r#"{"Push":{"moment":4,"kind":"settle","event":"released"}}"#
    );
    let Telemetry::Push(back) = serde_json::from_str(&json).unwrap() else {
        panic!("not a Push: {json}");
    };
    assert_eq!(back, record);
}
