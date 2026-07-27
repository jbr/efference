
use super::*;
use crate::api::Usage;
use crate::log::{CallId, Triage};

fn lines(s: &str) -> Vec<&str> {
    s.lines().collect()
}

/// A `Completion` whose assistant turn made a single `retain` call carrying
/// `input` — the shape `ForkPeek` forces and `parse_retain` reads.
fn retain_completion(name: &str, input: Value) -> Completion {
    Completion {
        assistant: Payload::Assistant {
            thinking: vec![],
            text: vec![],
            calls: vec![ToolCall {
                id: CallId("r".into()),
                name: name.into(),
                input,
            }],
        },
        model: "test".into(),
        stop_reason: None,
        usage: Usage {
            input_tokens: 0,
            output_tokens: 0,
            cache_creation_input_tokens: 0,
            cache_read_input_tokens: 0,
        },
    }
}

#[test]
fn peek_view_numbers_every_line() {
    // The throwaway fork's view — per-line numbers are the `retain` coordinate
    // system. (Only here; never on the canonical wire.)
    let ls = lines("alpha\nbeta\ngamma");
    let view = number_lines(&ls);
    assert!(view.contains("     1\talpha"), "{view}");
    assert!(view.contains("     3\tgamma"), "{view}");
}

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
    let p = parse_retain(&retain_completion(
        RETAIN,
        json!({"keep": "all", "note": "the gist"}),
    ))
    .expect("a retain call parses");
    assert_eq!(p.keep, Keep::All);
    assert_eq!(p.note.as_deref(), Some("the gist"));
    assert_eq!(
        p.raw_keep, "all",
        "the model's exact string is kept for audit"
    );
    assert!(p.inbox.is_empty(), "no inbox field → no dispositions");

    let p = parse_retain(&retain_completion(RETAIN, json!({"keep": "40-52,310-318"})))
        .expect("ranges parse");
    assert_eq!(p.keep, Keep::Ranges(vec![(40, 52), (310, 318)]));
    assert_eq!(p.raw_keep, "40-52,310-318");
    assert_eq!(p.note, None);
}

#[test]
fn parse_retain_reads_inbox_dispositions() {
    let p = parse_retain(&retain_completion(
        RETAIN,
        json!({ "keep": "all", "inbox": { "pin": [5, 7], "dismiss": [6] } }),
    ))
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

#[test]
fn parse_dispositions_reads_triage_inbox_and_background() {
    // The standalone idle-triage tool carries inbox + background, no `keep`.
    let c = retain_completion(
        TRIAGE,
        json!({
            "inbox": { "pin": [5], "dismiss": [6] },
            "background": { "pin": ["bg#1"], "mute": ["bg#2"] }
        }),
    );
    let (ibx, bg) = parse_dispositions(&c, TRIAGE).expect("a triage call parses");
    assert_eq!(ibx, vec![(5, Disposition::Pin), (6, Disposition::Dismiss)]);
    assert_eq!(
        bg,
        vec![
            ("bg#1".to_string(), BgVerdict::Pin),
            ("bg#2".to_string(), BgVerdict::Mute)
        ]
    );
    // A call by a different name yields nothing.
    assert!(parse_dispositions(&c, "something_else").is_none());
}

#[test]
fn parse_retain_is_none_without_a_retain_call() {
    // The peek is force-called to `retain`, but guard anyway: a turn that called
    // something else yields no verdict → keep-all fallback.
    assert!(parse_retain(&retain_completion("bash", json!({"command": "ls"}))).is_none());
}

#[test]
fn parse_retain_reads_surfaced_names() {
    // The `surfaced` field carries context-item names the model judged relevant
    // (docs/redaction-fork.md); parse_retain reads them unfiltered.
    let p = parse_retain(&retain_completion(
        RETAIN,
        json!({ "keep": "all", "surfaced": ["ferritin", "jsonl-prose"] }),
    ))
    .expect("parses");
    assert_eq!(p.surfaced, vec!["ferritin", "jsonl-prose"]);

    // Absent field → no surfacing (the lean default).
    let p = parse_retain(&retain_completion(RETAIN, json!({ "keep": "all" }))).unwrap();
    assert!(p.surfaced.is_empty());
}

fn entry(seq: u64, payload: Payload) -> LogEntry {
    use crate::log::Timestamp;
    LogEntry {
        seq,
        turn: 0,
        at: Timestamp(1000 + seq),
        payload,
    }
}

fn preamble_with(items: Vec<Item>) -> LogEntry {
    use crate::context::{Resolved, Section};
    entry(
        0,
        Payload::Preamble {
            system: "sys".into(),
            context: Resolved {
                sections: vec![Section {
                    heading: Some("Skills".into()),
                    items,
                }],
            },
        },
    )
}

fn listed(name: &str) -> Item {
    Item::Listed {
        name: name.into(),
        description: format!("what {name} does"),
        path: format!("skills/{name}.md"),
    }
}

#[test]
fn roster_lists_stub_items_and_excludes_inlined_and_exec() {
    // Only read-on-demand stubs (`Listed`) are surfaceable; inlined and exec bodies
    // are already whole on the wire, so there's nothing to load.
    let prefix = vec![preamble_with(vec![
        listed("ferritin"),
        Item::Inline {
            name: "CLAUDE".into(),
            path: "CLAUDE.md".into(),
            body: "the repo map".into(),
        },
        Item::Exec {
            name: "Env".into(),
            command: "env".into(),
            body: "PWD=/x".into(),
        },
        listed("jsonl-prose"),
    ])];
    assert_eq!(surfaceable_roster(&prefix), vec!["ferritin", "jsonl-prose"]);
}

#[test]
fn roster_excludes_already_surfaced_names() {
    // Surfaced-once dedup (docs/redaction-fork.md): a name any prior
    // `Triage.surfaced` carried drops out of the roster — folded from the log alone.
    let prefix = vec![
        preamble_with(vec![listed("ferritin"), listed("jsonl-prose")]),
        entry(
            1,
            Payload::Triage(Triage {
                for_result: Some(1),
                origin: Origin::Model,
                raw_keep: "all".into(),
                inbox: Vec::new(),
                surfaced: vec!["ferritin".into()],
            }),
        ),
    ];
    assert_eq!(
        surfaceable_roster(&prefix),
        vec!["jsonl-prose"],
        "ferritin was surfaced already; only jsonl-prose remains offerable"
    );
}

#[test]
fn roster_is_empty_without_a_preamble() {
    // No listing → nothing to offer (the pre-snapshot / config-less case).
    assert!(surfaceable_roster(&[]).is_empty());
}
