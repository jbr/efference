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
fn recorded_inference_comes_from_the_latest_boot_preamble() {
    let entry = |seq, inference: Option<LoggedInference>| LogEntry {
        seq,
        turn: 0,
        at: Timestamp::now(),
        submission_id: None,
        source: None,
        payload: Payload::Preamble {
            system: "sys".into(),
            context: Default::default(),
            inference,
        },
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
