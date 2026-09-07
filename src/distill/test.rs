use super::*;
use crate::api::Usage;
use crate::log::{CallId, Triage};
use crate::project::{
    ApiBlock, ApiMessage, BreakpointPlacement, ImagePlacement, Role, encode_messages, project_with,
};

/// The Messages-wire view of a fork tail, for assertions written against blocks.
fn wire(turns: &[WireTurn]) -> Vec<ApiMessage> {
    encode_messages(
        turns,
        ImagePlacement::default(),
        BreakpointPlacement::default(),
    )
}

fn lines(s: &str) -> Vec<&str> {
    s.lines().collect()
}

/// A `Completion` whose assistant turn made a single `retain` call carrying
/// `input` — the shape `ForkPeek` asks for and `parse_retain` reads.
/// The id of the result a single-result gate is triaging. Arbitrary — what matters is
/// that exactly one id is offered, which is the condition under which `parse_retain`
/// adopts the id-free shapes — the flat `keep`/`note`, or a lone mis-keyed entry.
const CALL_ID: &str = "toolu_x";

fn retain_completion(name: &str, input: Value) -> Completion {
    Completion {
        assistant: Payload::Assistant {
            output: AssistantOutput::from_legacy_messages(
                vec![],
                vec![],
                vec![ToolCall {
                    id: CallId("r".into()),
                    name: name.into(),
                    input,
                }],
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

/// The in-fork retry (docs/redaction-fork.md) must keep the fork conversation
/// API-valid: replay the non-compliant assistant turn, answer each of its calls with an
/// error `tool_result`, and renew the ask — so the retry extends the warm prefix
/// instead of opening a new request shape.
#[test]
fn a_retry_tail_answers_every_wrong_call_and_renews_the_ask() {
    let completion = retain_completion("bash", json!({"command": "ls"}));
    let mut messages = vec![WireTurn::text("the peek tail")];
    let why = unusable(&completion, RETAIN);
    assert!(extend_with_retry(
        &mut messages,
        &completion.assistant,
        &why,
        RETAIN
    ));
    assert_eq!(messages.len(), 3, "assistant turn + reply appended");
    let messages = wire(&messages);

    let assistant = &messages[1];
    assert_eq!(assistant.role, Role::Assistant);
    assert!(
        matches!(&assistant.content[..], [ApiBlock::ToolUse { id, name, .. }] if id == "r" && name == "bash"),
        "the wrong call is replayed verbatim"
    );

    let reply = &messages[2];
    assert_eq!(reply.role, Role::User);
    match &reply.content[..] {
        [
            ApiBlock::ToolResult {
                tool_use_id,
                is_error,
                content,
                ..
            },
            ApiBlock::Text { text, .. },
        ] => {
            assert_eq!(tool_use_id, "r", "the wrong call is answered, not orphaned");
            assert!(*is_error);
            assert!(
                content.contains("not accepted — peek called bash instead of retain"),
                "the error result carries the why: {content}"
            );
            assert!(text.contains("Call `retain` now"), "{text}");
        }
        other => panic!("expected [error tool_result, renewed ask], got {other:?}"),
    }
}

/// A prose-only non-compliant response (no tool call) still yields a valid retry tail:
/// the text turn is replayed and the renewed ask follows — no `tool_result` to pair.
#[test]
fn a_prose_only_response_retries_with_just_the_renewed_ask() {
    let completion = Completion {
        assistant: Payload::Assistant {
            output: AssistantOutput::from_legacy_messages(
                vec![],
                vec![crate::log::ContentBlock::Text("hmm, let me think".into())],
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
    };
    let mut messages = Vec::new();
    assert!(extend_with_retry(
        &mut messages,
        &completion.assistant,
        "peek called no tool",
        TRIAGE
    ));
    assert_eq!(messages.len(), 2);
    let messages = wire(&messages);
    assert!(
        matches!(&messages[1].content[..], [ApiBlock::Text { text, .. }] if text.contains("Call `triage` now"))
    );
}

/// An empty assistant payload has nothing appendable — terminal, not an invalid
/// (empty-content) message on the wire.
#[test]
fn an_empty_response_is_terminal_not_an_invalid_retry() {
    let empty = Payload::Assistant {
        output: AssistantOutput::Messages(vec![]),
        upstream_failure: None,
    };
    let mut messages = Vec::new();
    assert!(!extend_with_retry(
        &mut messages,
        &empty,
        "peek called no tool",
        RETAIN
    ));
    assert!(messages.is_empty());
}

/// Coverage is part of verdict validity (docs/redaction-fork.md): a verdict that
/// leaves any offered result unanswered is rejected — naming the missing ids so the
/// retry nudge is actionable — rather than letting the result fall through untriaged.
#[test]
fn an_incomplete_verdict_is_rejected_naming_the_missing_ids() {
    let offered = ["a".to_string(), "b".to_string()];
    let complete = parse_retain(
        &retain_completion(
            RETAIN,
            json!({ "results": [
                { "call_id": "a", "keep": "all" },
                { "call_id": "b", "keep": "none", "note": "n" },
            ] }),
        ),
        &offered,
    )
    .unwrap();
    assert!(require_coverage(complete, &offered).is_ok());

    let partial = parse_retain(
        &retain_completion(
            RETAIN,
            json!({ "results": [{ "call_id": "a", "keep": "all" }] }),
        ),
        &offered,
    )
    .unwrap();
    let Err(why) = require_coverage(partial, &offered) else {
        panic!("a partial verdict must be rejected");
    };
    assert!(why.contains("[b]"), "names the unanswered id: {why}");
}

/// Bounds are part of verdict validity (docs/redaction-fork.md): a `keep` range wholly
/// beyond the shown line count is a coordinate mistake — the shape it catches is a peek
/// adopting `grep -n`'s embedded file line numbers over the rendering's own — and
/// render-time normalization would drop it silently. The rejection names the bound
/// so the retry can re-index against the rendering.
#[test]
fn a_wholly_out_of_range_keep_is_rejected_naming_the_bound() {
    let offered = [CALL_ID.to_string()];
    let shown = [(CALL_ID.to_string(), 17)];
    let p = parse_retain(
        &retain_completion(RETAIN, json!({ "keep": "25-401" })),
        &offered,
    )
    .unwrap();
    let Err(why) = require_in_bounds(p, &shown) else {
        panic!("a wholly-out-of-range range must be rejected");
    };
    assert!(why.contains("25-401"), "names the range: {why}");
    assert!(why.contains("17"), "names the bound: {why}");

    // Overshoot from an in-bounds start is the peek's line accounting running a line
    // or two past the rendering's — not a coordinate mistake; render clamps the tail.
    let p = parse_retain(
        &retain_completion(RETAIN, json!({ "keep": "10-19" })),
        &offered,
    )
    .unwrap();
    assert!(require_in_bounds(p, &shown).is_ok());
}

/// One out-of-range range fails the whole verdict: `[[1,5],[425,430]]` would render
/// 1–5 and silently drop 425–430 — the same loss at partial scale.
#[test]
fn one_out_of_range_range_rejects_the_whole_verdict() {
    let offered = [CALL_ID.to_string()];
    let shown = [(CALL_ID.to_string(), 17)];
    let p = parse_retain(
        &retain_completion(RETAIN, json!({ "keep": "1-5,425-430" })),
        &offered,
    )
    .unwrap();
    let Err(why) = require_in_bounds(p, &shown) else {
        panic!("a mixed verdict with an out-of-range range must be rejected");
    };
    assert!(why.contains("425-430"), "names the offending range: {why}");
}

/// A 0 start is rejected, not clamped: `0-5` under zero-indexing means shown lines
/// 1–6, as a typo'd lower bound it means 1–5, and guessing either silently keeps the
/// wrong lines. The nudge restates the 1-based convention.
#[test]
fn a_zero_start_is_rejected_not_clamped() {
    let offered = [CALL_ID.to_string()];
    let shown = [(CALL_ID.to_string(), 17)];
    let p = parse_retain(
        &retain_completion(RETAIN, json!({ "keep": "0-5" })),
        &offered,
    )
    .unwrap();
    let Err(why) = require_in_bounds(p, &shown) else {
        panic!("a 0 start must be rejected");
    };
    assert!(why.contains("start at 1"), "restates the convention: {why}");
}

/// Bounds apply only where there are coordinates to check: `all`/`none` carry none,
/// and a verdict keyed to an id that was never offered has no bound — the caller's
/// reconciliation drops it, so it passes through here.
#[test]
fn bounds_skip_keywords_and_unoffered_ids() {
    let offered = ["a".to_string(), "b".to_string()];
    let shown = [("a".to_string(), 3), ("b".to_string(), 3)];
    let p = parse_retain(
        &retain_completion(
            RETAIN,
            json!({ "results": [
                { "call_id": "a", "keep": "all" },
                { "call_id": "b", "keep": "none" },
                { "call_id": "ghost", "keep": "900-901" },
            ] }),
        ),
        &offered,
    )
    .unwrap();
    assert!(require_in_bounds(p, &shown).is_ok());
}

/// A failed gate (docs/redaction-fork.md) withholds every result as a hard error —
/// `WireState::TriageFailed`, `Origin::Fallback`, the why in `raw_keep` — never the
/// untriaged raw.
#[test]
fn a_failed_gate_withholds_every_result_as_an_error() {
    let outcome = GateOutcome::failed(2, Vec::new(), "peek called no tool (attempt 3 of 3)");
    assert_eq!(outcome.results.len(), 2);
    for d in &outcome.results {
        assert!(
            matches!(&d.wire, WireState::TriageFailed { why } if why.contains("peek called no tool")),
            "withheld, not kept"
        );
        assert_eq!(d.origin, Origin::Fallback);
        assert!(d.raw_keep.contains("peek called no tool"));
    }
    assert!(outcome.inbox.is_empty() && outcome.background.is_empty());
}

/// The peek tools' schemas are static (docs/redaction-fork.md): they ride every turn's
/// `tools[]`, so any variation would re-key the request envelope and split the cache
/// lineage. Byte-identical across calls, every rider field unconditional, and the
/// standing description defers use until the harness asks.
#[test]
fn peek_tool_schemas_are_static_and_permission_phrased() {
    assert_eq!(
        serde_json::to_string(&retain_tool()).unwrap(),
        serde_json::to_string(&retain_tool()).unwrap()
    );
    let retain = retain_tool();
    for field in ["results", "inbox", "background", "surfaced"] {
        assert!(
            retain["input_schema"]["properties"].get(field).is_some(),
            "`{field}` is unconditional"
        );
    }
    let triage = triage_tool();
    for tool in [&retain, &triage] {
        let description = tool["description"].as_str().unwrap();
        assert!(
            description.contains("unless asked to do so by the harness"),
            "permission-phrased, not identity-phrased: {description}"
        );
    }
    assert!(is_peek_tool("retain") && is_peek_tool("triage"));
    assert!(!is_peek_tool("bash"));
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

fn entry(seq: u64, payload: Payload) -> LogEntry {
    use crate::log::Timestamp;
    LogEntry {
        seq,
        turn: 0,
        at: Timestamp(1000 + seq),
        submission_id: None,
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
                    description: None,
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
    assert_eq!(
        names(&surfaceable_roster(&prefix)),
        ["ferritin", "jsonl-prose"]
    );
}

/// The roster carries whole items now (so it can re-render them as the preamble did);
/// most assertions here are still about *which* items, by name.
fn names(roster: &[Item]) -> Vec<&str> {
    roster.iter().map(roster_name).collect()
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
                retries: Vec::new(),
                thinking: Vec::new(),
            }),
        ),
    ];
    assert_eq!(
        names(&surfaceable_roster(&prefix)),
        ["jsonl-prose"],
        "ferritin was surfaced already; only jsonl-prose remains offerable"
    );
}

#[test]
fn roster_is_empty_without_a_preamble() {
    // No listing → nothing to offer (the pre-snapshot / config-less case).
    assert!(surfaceable_roster(&[]).is_empty());
}

/// The invariant the batched gate exists to hold (docs/redaction-fork.md): when a
/// completion carries several calls, the peek's request must answer **every** one of
/// them.
///
/// Peeking between calls builds a request whose trailing assistant message still carries
/// unanswered `tool_use` blocks, which the API rejects. The whole gate then fails, and
/// because `WireState` is fixed once, whatever the failure path leaves behind stays in the
/// cached prefix for the rest of the session — one mis-timed peek is not recoverable later.
#[test]
fn a_gate_over_a_multi_call_step_answers_every_tool_use() {
    let calls = vec![
        ToolCall {
            id: CallId("search_2".into()),
            name: "search".into(),
            input: json!({}),
        },
        ToolCall {
            id: CallId("bash_3".into()),
            name: "bash".into(),
            input: json!({}),
        },
    ];
    let prefix = vec![
        entry(
            1,
            Payload::User(vec![crate::log::ContentBlock::Text("go".into())]),
        ),
        entry(
            2,
            Payload::Assistant {
                output: AssistantOutput::from_legacy_messages(vec![], vec![], calls.clone()),
                upstream_failure: None,
            },
        ),
    ];
    let raws = [
        RawResult {
            images: Vec::new(),
            content: "alpha\nbeta".into(),
            is_error: false,
            content_elided: None,
        },
        RawResult {
            images: Vec::new(),
            content: "gamma".into(),
            is_error: false,
            content_elided: None,
        },
    ];
    let pending: Vec<Pending<'_>> = calls
        .iter()
        .zip(&raws)
        .map(|(call, raw)| Pending { call, raw })
        .collect();

    let messages = wire(&peek_messages(&prefix, &pending, &[], &[], &[]));
    let tail = messages.last().expect("a tail message");
    assert_eq!(tail.role, Role::User);

    // Every `tool_use` in the trailing assistant message is answered, in call order.
    let answered: Vec<&str> = tail
        .content
        .iter()
        .filter_map(|b| match b {
            ApiBlock::ToolResult { tool_use_id, .. } => Some(tool_use_id.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(
        answered,
        vec!["search_2", "bash_3"],
        "both calls must be paired or the API rejects the request"
    );

    // Each result carries its own header and restarts numbering at 1, so the model
    // cannot address one result's lines under another's verdict.
    let views: Vec<&str> = tail
        .content
        .iter()
        .filter_map(|b| match b {
            ApiBlock::ToolResult { content, .. } => content.as_text(),
            _ => None,
        })
        .collect();
    assert!(views[0].contains("[search_2] search"), "{}", views[0]);
    assert!(views[1].contains("[bash_3] bash"), "{}", views[1]);
    assert!(views[0].contains("     1\talpha") && views[0].contains("     2\tbeta"));
    assert!(
        views[1].contains("     1\tgamma"),
        "the second result numbers from 1, not 3: {}",
        views[1]
    );

    // The gate-scoped riders and the instruction ride the trailing text block — they
    // belong to the step, not to any one result.
    assert!(matches!(tail.content.last(), Some(ApiBlock::Text { .. })));
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
                { "call_id": "a", "keep": "1-2", "note": "first" },
                { "call_id": "invented", "keep": "all" },
            ] }),
        ),
        &offered,
    )
    .expect("parses");
    assert_eq!(p.results.len(), 2);
    let a = p.results.iter().find(|v| v.call_id == "a").unwrap();
    assert_eq!(a.keep, Keep::Ranges(vec![(1, 2)]));
    assert_eq!(a.note.as_deref(), Some("first"));
    assert!(
        !p.results.iter().any(|v| v.call_id == "b"),
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
    assert_eq!(p.results[0].call_id, "only");
    assert_eq!(p.results[0].keep, Keep::All);

    let two = ["a".to_string(), "b".to_string()];
    let p = parse_retain(&retain_completion(RETAIN, json!({ "keep": "all" })), &two).unwrap();
    assert!(
        p.results.is_empty(),
        "ambiguous flat form must not be guessed onto a result"
    );
}

/// A single-result gate asks for the flat form and names no call id at all: copying an
/// id when only one result is in question is transcription with no decision in it, and
/// it is the token most likely to come back wrong (docs/redaction-fork.md).
#[test]
fn a_single_result_gate_asks_for_the_flat_form_and_no_call_id() {
    let instruction = retain_instruction(&["call_abc123".to_string()], &[]);
    assert!(
        !instruction.contains("call_abc123"),
        "nothing to copy: {instruction}"
    );
    assert!(instruction.contains("`keep` and `note`"));
    assert!(
        instruction.contains("no `results` array and no call id"),
        "names the shape: {instruction}"
    );
}

/// A multi-result gate asks for the keyed form and interpolates exactly the ids this
/// gate offered — the tail is discarded with the fork, so nothing here is cache-keyed.
#[test]
fn a_multi_result_gate_lists_every_offered_id_and_the_count() {
    let offered = ["call_a".to_string(), "call_b".to_string()];
    let instruction = retain_instruction(&offered, &[]);
    assert!(instruction.contains("[call_a], [call_b]"), "{instruction}");
    assert!(
        instruction.contains("exactly 2 `results` entries"),
        "{instruction}"
    );
}

/// Both shapes bound the ask to *this* gate. Every prior `tool_result` on the canonical
/// wire carries a `[seq]` block-addressing tag in the same bracket syntax as a
/// `[call id]` header, and an unbounded "every [call id] above" was read by a live model
/// as "re-triage the whole transcript" — twenty-odd entries keyed by `[seq]` tags and
/// invented ids, with the gate's own id missing, so coverage failed and the gate
/// hard-errored (docs/redaction-fork.md).
#[test]
fn every_retain_ask_excludes_the_transcript_behind_it() {
    for offered in [
        vec!["only".to_string()],
        vec!["a".to_string(), "b".to_string()],
    ] {
        let instruction = retain_instruction(&offered, &[]);
        assert!(
            instruction.contains("Nothing earlier in this conversation is being triaged"),
            "the ask must say what is out of scope: {instruction}"
        );
        assert!(
            instruction.contains("[n] tags are not call ids"),
            "and must disown the block-addressing tags: {instruction}"
        );
    }
}

/// The verdict's two halves are asked to agree with each other, not merely with the raw.
/// A `note` can state a fact that is true of the raw and unsupported by anything that
/// survives onto the wire — a multi-match search kept down to one line, with the note
/// describing a different match. A reader who catches that once re-verifies by hand from
/// then on, so the cost of an unauditable summary is paid on every later result too
/// (docs/redaction-fork.md).
#[test]
fn the_ask_grounds_the_note_in_the_kept_lines_and_flags_a_partial_enumeration() {
    for offered in [
        vec!["only".to_string()],
        vec!["a".to_string(), "b".to_string()],
    ] {
        let instruction = retain_instruction(&offered, &[]);
        assert!(
            instruction.contains("Ground it in\n              the lines you kept"),
            "the note must be answerable from what survives: {instruction}"
        );
        assert!(
            instruction.contains("that line belongs in `keep`"),
            "a claim about a line requires keeping that line: {instruction}"
        );
        assert!(
            instruction.contains("keep\n              the enumeration whole"),
            "a partial keep of an enumeration must be said, not implied: {instruction}"
        );
    }
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
            json!({ "results": [{ "call_id": "functions.read", "keep": "1-4", "note": "n" }] }),
        ),
        &one,
    )
    .unwrap();
    assert_eq!(p.results.len(), 1);
    assert_eq!(p.results[0].call_id, "only");
    assert_eq!(p.results[0].keep, Keep::Ranges(vec![(1, 4)]));
    assert!(require_coverage(p, &one).is_ok());

    // Two entries are ambiguous even on a single gate, where one is not.
    let p = parse_retain(
        &retain_completion(
            RETAIN,
            json!({ "results": [
                { "call_id": "wrong", "keep": "all" },
                { "call_id": "alsowrong", "keep": "none" },
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
            json!({ "results": [{ "call_id": "wrong", "keep": "all" }] }),
        ),
        &two,
    )
    .unwrap();
    assert!(
        !p.results
            .iter()
            .any(|v| v.call_id == "a" || v.call_id == "b")
    );
}

/// The coverage rejection names what the verdict *did* answer, not only what it missed.
/// "Answered nothing" and "answered a pile of ids that were never offered" are different
/// defects — the second is a prompt-scoping failure — and the breadcrumb is the only
/// place a later reader can tell them apart (docs/redaction-fork.md).
#[test]
fn a_coverage_rejection_reports_what_was_answered() {
    let offered = ["wanted".to_string()];
    let empty = parse_retain(
        &retain_completion(RETAIN, json!({ "results": [] })),
        &offered,
    )
    .unwrap();
    let Err(why) = require_coverage(empty, &offered) else {
        panic!("an empty verdict must be rejected");
    };
    assert!(
        why.contains("[wanted]") && why.contains("answered nothing"),
        "{why}"
    );

    let elsewhere = parse_retain(
        &retain_completion(
            RETAIN,
            json!({ "results": [
                { "call_id": "6", "keep": "all" },
                { "call_id": "multi_tool_use.parallel", "keep": "all" },
                { "call_id": "8", "keep": "all" },
            ] }),
        ),
        &offered,
    )
    .unwrap();
    let Err(why) = require_coverage(elsewhere, &offered) else {
        panic!("a verdict covering none of the offered ids must be rejected");
    };
    assert!(why.contains("[wanted]"), "{why}");
    assert!(
        why.contains("[6], [multi_tool_use.parallel], [8]"),
        "the wrong ids are the diagnosis: {why}"
    );
}

/// The ask opens on a banner and the imperative, and closes on the imperative. Every other
/// section of a peek view carries a banner; the ask carrying none left the only
/// *instruction* in the view reading as a trailing note. A session answered 29% of its
/// gates with a work tool instead of a verdict (journal/2026-08-07-session-key-review.md).
#[test]
fn the_ask_is_bannered_and_opens_and_closes_on_the_imperative() {
    let instruction = retain_instruction(&["call_x".to_string()], &[]);
    let mut lines = instruction.lines();
    assert_eq!(lines.next(), Some(ASK_BANNER), "{instruction}");
    assert!(
        instruction
            .lines()
            .nth(2)
            .is_some_and(|l| l.starts_with("Call `retain` now.")),
        "the imperative leads, not a description: {instruction}"
    );
    assert!(instruction.ends_with("Call `retain`."), "{instruction}");
}

/// The turn is stated to be a triage turn, and the closed toolbox is stated as fact — a
/// non-`retain` call really is answered with an error and the ask really does come back
/// (`extend_with_retry`), so the sentence is true as written rather than a discouragement.
#[test]
fn the_ask_says_this_turn_is_not_the_task() {
    let instruction = retain_instruction(&["call_x".to_string()], &[]);
    assert!(
        instruction.contains("not a work turn"),
        "the ask must disown the task: {instruction}"
    );
    assert!(
        instruction.contains("No other tool will run until you have called `retain`"),
        "and state the closed toolbox non-optionally: {instruction}"
    );
}

/// A rider's field row rides inside the block, and only when its section is above. An
/// absent rider must leave no trace — an unconditional mention would ask about a section
/// the model can't see.
#[test]
fn rider_fields_appear_only_when_their_section_does() {
    let offered = ["call_x".to_string()];
    let bare = retain_instruction(&offered, &[]);
    assert!(bare.contains("  keep "), "keep always rides: {bare}");
    for absent in ["surfaced", "inbox", "background"] {
        assert!(
            !bare.contains(absent),
            "{absent} named with no section: {bare}"
        );
    }
    let with_roster = retain_instruction(&offered, &[SURFACE_FIELD]);
    assert!(with_roster.contains("  surfaced "), "{with_roster}");
    assert!(!with_roster.contains("  inbox "), "{with_roster}");
}

/// The roster names each item **and its path**, so it reads as a file to open rather than
/// a capability to call — a session met a listed skill as a bare `[ferritin]` and built a
/// tool call out of the name (journal/2026-08-07-session-key-review.md).
///
/// It does *not* carry the summary. That is a measured call, not an aesthetic one: the
/// summaries are a quarter of a session's uncached fork tail and moved the first-attempt
/// failure rate not at all (journal/2026-08-07-triage-ask-measured.md).
#[test]
fn the_roster_gives_each_item_a_path_but_not_its_summary() {
    let roster = vec![listed("ferritin")];
    let mut view = String::new();
    append_roster(&mut view, &roster);
    assert!(view.contains("<ferritin"), "named: {view}");
    assert!(
        view.contains("path=\"skills/ferritin.md\""),
        "and located: {view}"
    );
    assert!(
        !view.contains("what ferritin does"),
        "the summary is the part that costs and doesn't pay: {view}"
    );
    assert!(!view.contains("[ferritin]"), "not the bare name: {view}");
}

/// The fork peek retries a stream that dies before finalizing on the **same budget and
/// schedule** as the main loop's own aborted turns (docs/transport.md) — one policy for
/// one failure, whichever surface it lands on. Before this, an aborted peek withheld on
/// the first abort while an aborted turn got four attempts; a controlled run caught the
/// asymmetry costing nine turns and a spurious conformance failure (docs/transport.md).
///
/// The cap is the part worth pinning: it is what lets the attempt count be large enough
/// to wait a flaky provider out without the tail of the schedule becoming a hang.
#[test]
fn an_aborted_peek_stream_backs_off_on_the_main_loops_budget() {
    use crate::agent::{STREAM_ABORT_ATTEMPTS, stream_abort_backoff};
    use std::time::Duration;
    assert_eq!(stream_abort_backoff(1), Some(Duration::from_secs(1)));
    assert_eq!(stream_abort_backoff(2), Some(Duration::from_secs(2)));
    assert_eq!(stream_abort_backoff(3), Some(Duration::from_secs(4)));
    // Capped, so a long budget does not end in one enormous sleep.
    assert_eq!(stream_abort_backoff(6), Some(Duration::from_secs(30)));
    assert_eq!(
        stream_abort_backoff(STREAM_ABORT_ATTEMPTS - 1),
        Some(Duration::from_secs(30))
    );
    // The last attempt exhausts the budget: no pause, withhold.
    assert_eq!(stream_abort_backoff(STREAM_ABORT_ATTEMPTS), None);
    assert_eq!(stream_abort_backoff(STREAM_ABORT_ATTEMPTS + 1), None);
    // The whole schedule is minutes, not hours.
    let total: u64 = (1..STREAM_ABORT_ATTEMPTS)
        .filter_map(stream_abort_backoff)
        .map(|d| d.as_secs())
        .sum();
    assert!((120..400).contains(&total), "whole schedule was {total}s");
}

// ---- the known-result-shape rule (docs/redaction-fork.md) ----

/// An fs-mcp `write` refusal: the rejected body is preserved server-side under a token
/// the model has to send back **verbatim** to reach it.
const HANDLE_RESULT: &str = "File src/main.rs already exists, use \"overwrite\": true if you \
intend to replace it.\n\nYour 8772 bytes of contents are preserved under \
`write-retry-9f3a1c04be27d5e8`: repeat the call with \"overwrite\": true and \"contents\": \
\"write-retry-9f3a1c04be27d5e8\" to confirm this exact write.";

fn call(id: &str, name: &str) -> ToolCall {
    ToolCall {
        id: CallId(id.into()),
        name: name.into(),
        input: json!({}),
    }
}

fn raw(content: &str, is_error: bool) -> RawResult {
    RawResult {
        images: Vec::new(),
        content: content.into(),
        is_error,
        content_elided: None,
    }
}

/// A gate carrying nothing but handle-bearing results is decided by the harness with no
/// request at all — the skipped verdict is also a skipped round-trip — and each result
/// projects `WireState::Raw`, so the token reaches the model exactly as the tool wrote it.
#[test]
fn a_gate_of_only_handle_bearing_results_is_decided_without_a_peek() {
    let call = call("write_1", "write");
    let raw = raw(HANDLE_RESULT, true);
    let pending = vec![Pending {
        call: &call,
        raw: &raw,
    }];
    let outcome = bypassed_gate(&pending).expect("every result is bypassed");
    assert_eq!(outcome.results.len(), 1);
    assert!(matches!(outcome.results[0].wire, WireState::Raw));
    assert_eq!(outcome.results[0].origin, Origin::Fallback);
    assert!(
        outcome.results[0]
            .raw_keep
            .contains("fs-mcp write retry token"),
        "the breadcrumb names the shape, which is what separates a bypass from a \
         failed gate in the log: {}",
        outcome.results[0].raw_keep
    );
    // No request was made, so there is nothing to bill and no retry to record.
    assert!(outcome.usage.is_empty() && outcome.retries.is_empty());
}

/// An ordinary result is still the fork's: no bypass, and it is offered for a verdict.
#[test]
fn an_ordinary_result_still_goes_through_the_fork() {
    let call = call("read_1", "read");
    let raw = raw("alpha\nbeta\n", false);
    let pending = vec![Pending {
        call: &call,
        raw: &raw,
    }];
    assert!(bypassed_gate(&pending).is_none());
    assert_eq!(offered_ids(&pending), vec!["read_1".to_string()]);
}

/// A mixed gate still runs — the handle-bearing result is answered (an unpaired
/// `tool_use` makes the request invalid) but with a stand-in, and only the other id is
/// offered, so the peek is never asked to decide the one the harness already decided.
#[test]
fn a_handle_bearing_result_is_answered_but_never_offered() {
    let calls = [call("write_1", "write"), call("read_2", "read")];
    let raws = [raw(HANDLE_RESULT, true), raw("alpha\nbeta\n", false)];
    let prefix = vec![
        entry(
            1,
            Payload::User(vec![crate::log::ContentBlock::Text("go".into())]),
        ),
        entry(
            2,
            Payload::Assistant {
                output: AssistantOutput::from_legacy_messages(vec![], vec![], calls.to_vec()),
                upstream_failure: None,
            },
        ),
    ];
    let pending: Vec<Pending<'_>> = calls
        .iter()
        .zip(&raws)
        .map(|(call, raw)| Pending { call, raw })
        .collect();

    assert!(
        bypassed_gate(&pending).is_none(),
        "one result still needs a verdict"
    );
    assert_eq!(offered_ids(&pending), vec!["read_2".to_string()]);

    let messages = wire(&peek_messages(&prefix, &pending, &[], &[], &[]));
    let tail = messages.last().expect("a tail message");
    let blocks: Vec<(&str, &str)> = tail
        .content
        .iter()
        .filter_map(|b| match b {
            ApiBlock::ToolResult {
                tool_use_id,
                content,
                ..
            } => Some((tool_use_id.as_str(), content.as_text().unwrap_or_default())),
            _ => None,
        })
        .collect();
    assert_eq!(
        blocks.iter().map(|(id, _)| *id).collect::<Vec<_>>(),
        vec!["write_1", "read_2"],
        "both calls are paired or the API rejects the request"
    );
    assert!(
        blocks[0].1.contains("not triaged") && !blocks[0].1.contains("write-retry-"),
        "the bypassed result answers with a stand-in, not its output: {}",
        blocks[0].1
    );
    assert!(blocks[1].1.contains("     1\talpha"));

    let instruction = match tail.content.last() {
        Some(ApiBlock::Text { text, .. }) => text.as_str(),
        other => panic!("the ask rides the trailing text block, got {other:?}"),
    };
    assert!(
        !instruction.contains("write_1"),
        "the bypassed id is not offered: {instruction}"
    );
    assert!(
        instruction.contains("the one call you just made"),
        "one offered id means the single-result shape: {instruction}"
    );
}

/// End to end: a bypassed result's `WireState::Raw` projects the whole body onto the
/// canonical wire, token intact. This is the property the rule exists for — a
/// summarized token is unusable, and the model is told to quote it back.
#[test]
fn a_bypassed_result_reaches_the_wire_verbatim() {
    let verdict = bypass_verdict("fs-mcp write retry token");
    let log = vec![
        entry(
            1,
            Payload::User(vec![crate::log::ContentBlock::Text("go".into())]),
        ),
        entry(
            2,
            Payload::Assistant {
                output: AssistantOutput::from_legacy_messages(
                    vec![],
                    vec![],
                    vec![call("write_1", "write")],
                ),
                upstream_failure: None,
            },
        ),
        entry(
            3,
            Payload::ToolResult {
                call_id: CallId("write_1".into()),
                raw: raw(HANDLE_RESULT, true),
                wire: verdict.wire,
            },
        ),
    ];
    let wire = project_with(&log, ImagePlacement::default());
    let result = wire
        .iter()
        .flat_map(|m| &m.content)
        .find_map(|b| match b {
            ApiBlock::ToolResult { content, .. } => content.as_text(),
            _ => None,
        })
        .expect("the result is on the wire");
    assert!(
        result.contains("write-retry-9f3a1c04be27d5e8"),
        "the token survives to the wire: {result}"
    );
    assert!(
        result.ends_with(HANDLE_RESULT),
        "and so does the rest of the refusal, byte for byte: {result}"
    );
}

/// What the gate records as the peek's reasoning (docs/redaction-fork.md): every thinking
/// block's text, joined — prose and redacted blocks excluded (a redacted block carries no
/// text), and `None` when the attempt thought nothing, so a gate that didn't think records
/// nothing rather than an empty entry. Signatures are dropped: this is a record, not a
/// replay input.
#[test]
fn peek_thinking_joins_thinking_text_only() {
    use crate::log::ContentBlock;

    let assistant = |thinking: Vec<ContentBlock>| Payload::Assistant {
        output: AssistantOutput::from_legacy_messages(
            thinking,
            vec![ContentBlock::Text("prose the gate never reads".into())],
            Vec::new(),
        ),
        upstream_failure: None,
    };

    assert_eq!(peek_thinking(&assistant(Vec::new())), None);
    assert_eq!(
        peek_thinking(&assistant(vec![
            ContentBlock::Thinking {
                text: "the head is boilerplate".into(),
                signature: "sig-a".into(),
            },
            ContentBlock::RedactedThinking {
                data: "opaque".into(),
            },
            ContentBlock::Thinking {
                text: "keep 40-52".into(),
                signature: "sig-b".into(),
            },
        ])),
        Some("the head is boilerplate\nkeep 40-52".to_string())
    );
    assert_eq!(
        peek_thinking(&assistant(vec![ContentBlock::RedactedThinking {
            data: "opaque".into(),
        }])),
        None,
        "a redacted-only attempt has no text to record"
    );
}

/// The Responses analogue of the canonical-versus-peek prefix rule
/// (docs/redaction-fork.md): under explicit caching the peek's one marker sits at the
/// shared pre-fork frontier, nothing in the fork-local raw tail carries one, and the
/// next canonical request is that same prefix, content for content, followed by the
/// distillate — never the raw output or the peek's ask.
#[test]
fn a_responses_peek_marks_the_shared_frontier_and_the_canonical_wire_carries_only_the_distillate() {
    use crate::api::{PromptCachePolicy, encode_responses};
    use crate::log::{Keep, ResponsesOutputItem, WireState};

    let call = ToolCall {
        id: CallId("call_1".into()),
        name: "bash".into(),
        input: json!({ "command": "ls" }),
    };
    let prefix = vec![
        entry(
            1,
            Payload::User(vec![crate::log::ContentBlock::Text("go".into())]),
        ),
        entry(
            2,
            Payload::Assistant {
                output: AssistantOutput::Responses(vec![
                    ResponsesOutputItem::Reasoning {
                        id: "rs_1".into(),
                        status: Some("completed".into()),
                        summary: Vec::new(),
                        content: Vec::new(),
                        encrypted_content: Some("ENC==".into()),
                    },
                    ResponsesOutputItem::FunctionCall {
                        id: Some("fc_1".into()),
                        call_id: call.id.clone(),
                        name: call.name.clone(),
                        arguments: r#"{"command":"ls"}"#.into(),
                        status: Some("completed".into()),
                    },
                ]),
                upstream_failure: None,
            },
        ),
    ];
    let raw = raw("alpha\nbeta\ngamma", false);
    let pending = [Pending {
        call: &call,
        raw: &raw,
    }];

    // The peek: prefix, frontier, then the fork-local raw and the ask.
    let peek = peek_request(&prefix, &pending, &[], &[]);
    let peek_input = encode_responses(&peek, "", PromptCachePolicy::Explicit);
    let peek_json: Vec<serde_json::Value> = peek_input
        .iter()
        .map(|item| serde_json::to_value(item).unwrap())
        .collect();
    let marked: Vec<usize> = peek_json
        .iter()
        .enumerate()
        .filter(|(_, item)| item.to_string().contains("prompt_cache_breakpoint"))
        .map(|(i, _)| i)
        .collect();
    let first_output = peek_json
        .iter()
        .position(|item| item["type"] == "function_call_output")
        .expect("the raw tail answers the call");
    assert_eq!(marked.len(), 1, "one marker: {peek_json:#?}");
    assert!(
        marked[0] < first_output,
        "the marker sits before the fork-local tail, not on it: marked {marked:?}, tail at {first_output}"
    );
    let tail = serde_json::to_string(&peek_json[first_output..]).unwrap();
    assert!(tail.contains("gamma"), "the raw rides the tail: {tail}");
    let shared = serde_json::to_string(&peek_json[..first_output]).unwrap();
    assert!(!shared.contains("gamma"), "and never the prefix: {shared}");

    // The canonical continuation: the verdict is logged, the raw is not projected.
    let mut log = prefix;
    log.push(entry(
        3,
        Payload::ToolResult {
            call_id: call.id.clone(),
            raw: raw.clone(),
            wire: WireState::Distilled {
                keep: Keep::Ranges(vec![(1, 1)]),
                note: Some("kept the first".into()),
            },
        },
    ));
    let mut canonical = project_semantic(&log);
    canonical.push(WireTurn::Breakpoint);
    let canonical_input = encode_responses(&canonical, "", PromptCachePolicy::Explicit);
    let canonical_json: Vec<serde_json::Value> = canonical_input
        .iter()
        .map(|item| serde_json::to_value(item).unwrap())
        .collect();
    // The pre-fork items are the peek's, content for content. The marker is a
    // write instruction, not content: the peek's sits at the frontier, the
    // canonical request's at its own tail, and the provider matches the prefix on
    // what is between them (docs/log-and-projection.md).
    fn unmarked(item: &serde_json::Value) -> serde_json::Value {
        match item {
            serde_json::Value::Object(map) => map
                .iter()
                .filter(|(key, _)| key.as_str() != "prompt_cache_breakpoint")
                .map(|(key, value)| (key.clone(), unmarked(value)))
                .collect(),
            serde_json::Value::Array(items) => items.iter().map(unmarked).collect(),
            other => other.clone(),
        }
    }
    assert_eq!(
        canonical_json[..first_output]
            .iter()
            .map(unmarked)
            .collect::<Vec<_>>(),
        peek_json[..first_output]
            .iter()
            .map(unmarked)
            .collect::<Vec<_>>(),
        "the pre-fork items are the peek's — one cache lineage"
    );
    let canonical_marked: Vec<usize> = canonical_json
        .iter()
        .enumerate()
        .filter(|(_, item)| item.to_string().contains("prompt_cache_breakpoint"))
        .map(|(i, _)| i)
        .collect();
    assert_eq!(
        canonical_marked,
        vec![canonical_json.len() - 1],
        "the canonical marker moved to the distillate at the tail: {canonical_json:#?}"
    );
    let text = serde_json::to_string(&canonical_json).unwrap();
    assert!(
        text.contains("alpha") && text.contains("kept the first"),
        "{text}"
    );
    assert!(
        !text.contains("gamma"),
        "dropped raw stays off the canonical wire: {text}"
    );
    let ask = peek_json.last().unwrap().to_string();
    assert!(
        ask.contains("retain"),
        "the peek's ask names the retain tool: {ask}"
    );
    let ask_text = peek_json.last().unwrap()["content"][0]["text"]
        .as_str()
        .expect("the ask is a text part");
    assert!(
        !text.contains(ask_text),
        "the peek's ask never reaches the canonical wire"
    );
}

/// The peek's request surface follows its configuration (docs/redaction-fork.md):
/// shared borrows the agent's client and `tools[]`; own sends its own client and
/// only the two peek tools.
#[test]
fn own_peek_sends_its_own_client_and_only_the_peek_tools() {
    let resolved = std::collections::BTreeMap::from([(
        "ANTHROPIC_API_KEY".to_string(),
        "sk-ant-xyz".to_string(),
    )]);
    let endpoint = crate::api::Endpoint::from_env_with(&resolved).unwrap();
    let agent = Client::new(endpoint.clone(), "big", "sys", None);
    let agent_tools = vec![json!({"name": "read"}), retain_tool(), triage_tool()];

    let shared = ForkPeek::shared();
    assert!(!shared.is_own());
    assert_eq!(shared.own_model(), None);
    assert_eq!(shared.client(&agent).model(), "big");
    assert_eq!(shared.tools(&agent_tools).len(), 3);

    let own = ForkPeek::own(Client::new(endpoint.for_triage(), "small", "persona", None));
    assert!(own.is_own());
    assert_eq!(own.own_model(), Some("small"));
    assert_eq!(own.client(&agent).model(), "small");
    let names: Vec<&str> = own
        .tools(&agent_tools)
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["retain", "triage"]);

    // The own peek forces the asked-for tool; the shared peek never does, and the
    // knob turns the own one back to `auto`.
    assert_eq!(shared.choice(RETAIN).forced(), None);
    assert_eq!(own.choice(RETAIN).forced(), Some("retain"));
    assert_eq!(own.choice(TRIAGE).forced(), Some("triage"));
    assert_eq!(own.with_auto_choice().choice(RETAIN).forced(), None);
}
