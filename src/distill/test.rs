use super::*;
use crate::api::Usage;
use crate::log::{CallId, Triage};

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
            thinking: vec![],
            text: vec![],
            calls: vec![ToolCall {
                id: CallId("r".into()),
                name: name.into(),
                input,
            }],
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
    let mut messages = vec![ApiMessage {
        role: Role::User,
        content: vec![ApiBlock::Text {
            text: "the peek tail".into(),
            cache_control: None,
        }],
    }];
    let why = unusable(&completion, RETAIN);
    assert!(extend_with_retry(
        &mut messages,
        &completion.assistant,
        &why,
        RETAIN
    ));
    assert_eq!(messages.len(), 3, "assistant turn + reply appended");

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
            thinking: vec![],
            text: vec![crate::log::ContentBlock::Text("hmm, let me think".into())],
            calls: vec![],
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
    };
    let mut messages = Vec::new();
    assert!(extend_with_retry(
        &mut messages,
        &completion.assistant,
        "peek called no tool",
        TRIAGE
    ));
    assert_eq!(messages.len(), 2);
    assert!(
        matches!(&messages[1].content[..], [ApiBlock::Text { text, .. }] if text.contains("Call `triage` now"))
    );
}

/// An empty assistant payload has nothing appendable — terminal, not an invalid
/// (empty-content) message on the wire.
#[test]
fn an_empty_response_is_terminal_not_an_invalid_retry() {
    let empty = Payload::Assistant {
        thinking: vec![],
        text: vec![],
        calls: vec![],
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
/// This is the regression guard for the observed failure. Peeking between calls built a
/// request whose trailing assistant message still carried unanswered `tool_use` blocks;
/// the API rejected it, `ForkPeek` fell back to keep-all (the fallback of the time —
/// today the same failure would withhold the results as hard errors), and — because
/// `WireState` is fixed once — the uncurated result stayed in the cached prefix for the
/// rest of the session. In the corpus that accounted for the first N−1 peeks of every
/// multi-call turn.
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
                thinking: vec![],
                text: vec![],
                calls: calls.clone(),
            },
        ),
    ];
    let raws = [
        RawResult {
            content: "alpha\nbeta".into(),
            is_error: false,
        },
        RawResult {
            content: "gamma".into(),
            is_error: false,
        },
    ];
    let pending: Vec<Pending<'_>> = calls
        .iter()
        .zip(&raws)
        .map(|(call, raw)| Pending { call, raw })
        .collect();

    let messages = peek_messages(&prefix, &pending, &[], &[], &[]);
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
            ApiBlock::ToolResult { content, .. } => Some(content.as_str()),
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

    // Two entries is no longer unambiguous, even on a single gate.
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

/// The roster renders each item exactly as the boot preamble spelled it, through the
/// preamble's own renderer — summary and path included. A bare `[name]` under-describes
/// what the item is: a session met a listed skill that way and built a tool call out of
/// the name rather than reading the file (journal/2026-08-07-session-key-review.md).
#[test]
fn the_roster_renders_items_as_the_preamble_spelled_them() {
    let roster = vec![listed("ferritin")];
    let mut view = String::new();
    append_roster(&mut view, &roster);
    assert!(
        view.contains(&crate::context::render_item(&roster[0])),
        "one renderer, so the two can't drift: {view}"
    );
    assert!(view.contains("path=\"skills/ferritin.md\""), "{view}");
    assert!(view.contains("what ferritin does"), "{view}");
    assert!(!view.contains("[ferritin]"), "not the bare name: {view}");
}
