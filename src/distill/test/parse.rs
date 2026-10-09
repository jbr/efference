//! Tests for parsing the peek's answer back.
use super::*;

#[test]
fn parse_keep_handles_keywords_ranges_and_garbage() {
    assert_eq!(parse_keep("all"), Keep::All);
    assert_eq!(parse_keep(" ALL "), Keep::All);
    assert_eq!(parse_keep("none"), Keep::None);
    assert_eq!(parse_keep(""), Keep::None);
    assert_eq!(parse_keep("7"), Keep::Ranges(vec![(7, 7)]));
    assert_eq!(
        parse_keep(" 40-52 , 310-318 "),
        Keep::Ranges(vec![(40, 52), (310, 318)])
    );
    assert_eq!(parse_keep("just words"), Keep::None);
}

#[test]
fn parse_retain_reads_keep_note_and_raw_keep() {
    let offered = [CALL_ID.to_string()];
    let p = parse_retain(
        &retain_completion(RETAIN, json!({"keep": "all", "note": "the gist"})),
        &offered,
    )
    .expect("a retain call parses");
    assert_eq!(p.results[0].keep, Keep::All);
    assert_eq!(p.results[0].note.as_deref(), Some("the gist"));
    assert_eq!(
        p.results[0].raw_keep, "all",
        "the model's exact string is kept for audit"
    );
    assert!(p.inbox.is_empty(), "no inbox field → no dispositions");

    let p = parse_retain(
        &retain_completion(RETAIN, json!({"keep": "40-52,310-318"})),
        &offered,
    )
    .expect("ranges parse");
    assert_eq!(p.results[0].keep, Keep::Ranges(vec![(40, 52), (310, 318)]));
    assert_eq!(p.results[0].raw_keep, "40-52,310-318");
    assert_eq!(p.results[0].note, None);
}

#[test]
fn parse_retain_reads_inbox_dispositions() {
    let offered = [CALL_ID.to_string()];
    let p = parse_retain(
        &retain_completion(
            RETAIN,
            json!({ "keep": "all", "inbox": { "pin": [5, 7], "dismiss": [6] } }),
        ),
        &offered,
    )
    .expect("parses");
    assert_eq!(
        p.inbox,
        vec![
            (5, Disposition::Pin),
            (7, Disposition::Pin),
            (6, Disposition::Dismiss)
        ]
    );
}

/// Notification ids arrive stringified: the peek reads them out of rendered
/// prose, and a live fork answer whose only defect was `"pin": ["102"]` cost
/// a full retry. `"102"` and `102` parse alike; a
/// non-numeric string still fails the whole input.
#[test]
fn parse_retain_accepts_stringified_notification_ids() {
    let offered = [CALL_ID.to_string()];
    let p = parse_retain(
        &retain_completion(
            RETAIN,
            json!({
                "background": { "mute": [], "pin": [] },
                "inbox": { "dismiss": [], "pin": ["102"] },
                "keep": "4-19",
                "note": "n"
            }),
        ),
        &offered,
    )
    .expect("stringified ids parse");
    assert_eq!(p.inbox, vec![(102, Disposition::Pin)]);

    let bad = parse_retain(
        &retain_completion(
            RETAIN,
            json!({ "keep": "all", "inbox": { "pin": ["p#8"] } }),
        ),
        &offered,
    );
    assert!(
        bad.is_none(),
        "a non-numeric id is invention, not transcription"
    );
}

#[test]
fn parse_dispositions_reads_triage_inbox_and_background() {
    // The standalone idle-triage tool carries inbox + background, no `keep`.
    let c = retain_completion(
        TRIAGE,
        json!({
            "inbox": { "pin": [5], "dismiss": [6] },
            "background": { "pin": ["p#1"], "mute": ["p#2"] }
        }),
    );
    let (ibx, bg) = parse_dispositions(&c, TRIAGE).expect("a triage call parses");
    assert_eq!(ibx, vec![(5, Disposition::Pin), (6, Disposition::Dismiss)]);
    assert_eq!(
        bg,
        vec![
            ("p#1".to_string(), BgVerdict::Pin),
            ("p#2".to_string(), BgVerdict::Mute)
        ]
    );
    // A call by a different name yields nothing.
    assert!(parse_dispositions(&c, "something_else").is_none());
}

#[test]
fn parse_retain_is_none_without_a_retain_call() {
    // The peek asks for `retain` via its tail instruction (never a forced
    // tool_choice), so a turn that called something else yields no verdict — the
    // in-fork retry's trigger.
    assert!(parse_retain(&retain_completion("bash", json!({"command": "ls"})), &[]).is_none());
}

#[test]
fn parse_retain_reads_surfaced_names() {
    let offered = [CALL_ID.to_string()];
    // The `surfaced` field carries context-item names the model judged relevant
    // (docs/redaction-fork.md); parse_retain reads them unfiltered.
    let p = parse_retain(
        &retain_completion(
            RETAIN,
            json!({ "keep": "all", "surfaced": ["ferritin", "jsonl-prose"] }),
        ),
        &offered,
    )
    .expect("parses");
    assert_eq!(p.surfaced, vec!["ferritin", "jsonl-prose"]);

    // Absent field → no surfacing (the lean default).
    let p = parse_retain(
        &retain_completion(RETAIN, json!({ "keep": "all" })),
        &offered,
    )
    .unwrap();
    assert!(p.surfaced.is_empty());
}

/// A verdict addressed to a call that was never offered finds no home, and the result it
/// failed to cover is reported as unanswered rather than silently keeping nothing.
#[test]
fn parse_retain_keys_verdicts_by_call_id() {
    let offered = ["a".to_string(), "b".to_string()];
    let p = parse_retain(
        &retain_completion(
            RETAIN,
            json!({ "results": [
                { "result": "a", "keep": "1-2", "note": "first" },
                { "result": "invented", "keep": "all" },
            ] }),
        ),
        &offered,
    )
    .expect("parses");
    assert_eq!(p.results.len(), 2);
    let a = p.results.iter().find(|v| v.label == "a").unwrap();
    assert_eq!(a.keep, Keep::Ranges(vec![(1, 2)]));
    assert_eq!(a.note.as_deref(), Some("first"));
    assert!(
        !p.results.iter().any(|v| v.label == "b"),
        "`b` was offered but never addressed — the caller keeps it uncurated, loudly"
    );
}

/// The flat form is adopted only when it is unambiguous — exactly one result in
/// flight. With several offered there is no way to know which it meant, so it is dropped
/// rather than guessed onto the wrong result.
#[test]
fn flat_keep_is_adopted_only_for_a_single_result_gate() {
    let one = ["only".to_string()];
    let p = parse_retain(&retain_completion(RETAIN, json!({ "keep": "all" })), &one).unwrap();
    assert_eq!(p.results.len(), 1);
    assert_eq!(p.results[0].label, "only");
    assert_eq!(p.results[0].keep, Keep::All);

    let two = ["a".to_string(), "b".to_string()];
    let p = parse_retain(&retain_completion(RETAIN, json!({ "keep": "all" })), &two).unwrap();
    assert!(
        p.results.is_empty(),
        "ambiguous flat form must not be guessed onto a result"
    );
}

/// A lone keyed entry whose `call_id` doesn't match is re-keyed on a single-result gate:
/// with one target there is nothing to mis-attribute *to*, so withholding output the
/// model plainly triaged over a mistyped id buys nothing. With several offered the same
/// entry stays where the model put it — there the id is the only attribution there is.
#[test]
fn a_lone_mis_keyed_verdict_is_re_keyed_only_for_a_single_result_gate() {
    let one = ["only".to_string()];
    let p = parse_retain(
        &retain_completion(
            RETAIN,
            json!({ "results": [{ "result": "functions.read", "keep": "1-4", "note": "n" }] }),
        ),
        &one,
    )
    .unwrap();
    assert_eq!(p.results.len(), 1);
    assert_eq!(p.results[0].label, "only");
    assert_eq!(p.results[0].keep, Keep::Ranges(vec![(1, 4)]));
    assert!(require_coverage(p, &one).is_ok());

    // Two entries are ambiguous even on a single gate, where one is not.
    let p = parse_retain(
        &retain_completion(
            RETAIN,
            json!({ "results": [
                { "result": "wrong", "keep": "all" },
                { "result": "alsowrong", "keep": "none" },
            ] }),
        ),
        &one,
    )
    .unwrap();
    assert!(require_coverage(p, &one).is_err());

    let two = ["a".to_string(), "b".to_string()];
    let p = parse_retain(
        &retain_completion(
            RETAIN,
            json!({ "results": [{ "result": "wrong", "keep": "all" }] }),
        ),
        &two,
    )
    .unwrap();
    assert!(!p.results.iter().any(|v| v.label == "a" || v.label == "b"));
}

/// A `Completion` whose assistant turn is prose only — the title peek's answer
/// shape (docs/session-title.md).
fn text_completion(text: &str) -> Completion {
    Completion {
        receipt: Default::default(),
        assistant: Payload::Assistant {
            output: AssistantOutput::from_legacy_messages(
                vec![],
                vec![crate::log::ContentBlock::Text(text.into())],
                vec![],
            ),
            upstream_failure: None,
        },
        id: None,
        model: "test".into(),
        stop_reason: None,
        usage: Usage {
            input_tokens: 0,
            output_tokens: 0,
            cache_creation_input_tokens: 0,
            cache_read_input_tokens: 0,
        },
        failure_class: crate::api::FailureClass::Transient,
    }
}

/// The title is the first non-empty text line, unwrapped (docs/session-title.md):
/// quotes, backticks, a `Title:` label, and a trailing period are the model's
/// framing, not the title. A tool call, no text, or an over-long line is unusable,
/// and the `Err` is the retry nudge.
#[test]
fn parse_title_takes_the_first_line_unwrapped_and_rejects_the_unusable() {
    for (raw, want) in [
        (
            "Session names: slugs and titles",
            "Session names: slugs and titles",
        ),
        (
            "\"Session names: slugs and titles.\"",
            "Session names: slugs and titles",
        ),
        ("`Session names`\n\nsecond line ignored", "Session names"),
        ("\n  Title: Session names  \n", "Session names"),
        ("**Session names**", "Session names"),
    ] {
        assert_eq!(
            parse_title(&text_completion(raw)).as_deref(),
            Ok(want),
            "{raw:?}"
        );
    }
    assert!(parse_title(&text_completion("")).is_err());
    assert!(parse_title(&text_completion("\"\"")).is_err());
    assert!(parse_title(&text_completion(&"x".repeat(TITLE_MAX_CHARS + 1))).is_err());
    assert!(
        parse_title(&retain_completion("retain", json!({"keep": "1"}))).is_err(),
        "a tool call is not a title"
    );
}
