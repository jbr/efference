//! Tests for the fork's triage mechanics, including the known-result-shape rule
//! (docs/triage.md).
use super::*;

/// The invariant the batched triage exists to hold (docs/triage.md): when a
/// completion carries several calls, the triage's request must answer **every** one of
/// them.
///
/// Peeking between calls builds a request whose trailing assistant message still carries
/// unanswered `tool_use` blocks, which the API rejects. The whole triage then fails, and
/// because `WireState` is fixed once, whatever the failure path leaves behind stays in the
/// cached prefix for the rest of the session — one mis-timed triage is not recoverable later.
#[test]
fn a_gate_over_a_multi_call_step_answers_every_tool_use() {
    let calls = vec![
        ToolCall::new("search", "search_2", json!({})),
        ToolCall::new("bash", "bash_3", json!({})),
    ];
    let prefix = vec![
        entry(1, Payload::user("go")),
        entry(
            2,
            Payload::assistant(AssistantOutput::from_legacy_messages(
                vec![],
                vec![],
                calls.clone(),
            )),
        ),
    ];
    let raws = [RawResult::text("alpha\nbeta"), RawResult::text("gamma")];
    let pending: Vec<Pending<'_>> = calls
        .iter()
        .zip(&raws)
        .map(|(call, raw)| Pending { call, raw })
        .collect();

    let messages = wire(&triage_messages(
        &prefix,
        Reader::Main,
        &pending,
        &[],
        &[],
        &[],
    ));
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

    // Each result carries its own labelled header and restarts numbering at 1, so the
    // model cannot address one result's lines under another's verdict. The provider's
    // call id stays on the pairing and out of the view: the verdict answers by label.
    let views: Vec<&str> = tail
        .content
        .iter()
        .filter_map(|b| match b {
            ApiBlock::ToolResult { content, .. } => content.as_text(),
            _ => None,
        })
        .collect();
    assert!(
        views[0].starts_with("═══ result r1 · search ═══"),
        "{}",
        views[0]
    );
    assert!(
        views[1].starts_with("═══ result r2 · bash ═══"),
        "{}",
        views[1]
    );
    assert!(
        !views
            .iter()
            .any(|v| v.contains("search_2") || v.contains("bash_3")),
        "{views:?}"
    );
    assert!(views[0].contains("   1 │ alpha") && views[0].contains("   2 │ beta"));
    assert!(
        views[1].contains("   1 │ gamma"),
        "the second result numbers from 1, not 3: {}",
        views[1]
    );

    // The triage-scoped riders and the instruction ride the trailing text block — they
    // belong to the step, not to any one result.
    assert!(matches!(tail.content.last(), Some(ApiBlock::Text { .. })));
}

/// An fs-mcp `write` refusal: the rejected body is preserved server-side under a token
/// the model has to send back **verbatim** to reach it.
const HANDLE_RESULT: &str = "File src/main.rs already exists, use \"overwrite\": true if you \
intend to replace it.\n\nYour 8772 bytes of contents are preserved under \
`write-retry-9f3a1c04be27d5e8`: repeat the call with \"overwrite\": true and \"contents\": \
\"write-retry-9f3a1c04be27d5e8\" to confirm this exact write.";

/// A triage carrying nothing but handle-bearing results is decided by the harness with no
/// request at all — the skipped verdict is also a skipped round-trip — and each result
/// projects `WireState::Raw`, so the token reaches the model exactly as the tool wrote it.
#[test]
fn a_gate_of_only_handle_bearing_results_is_decided_without_a_triage() {
    let call = ToolCall::new("write", "write_1", json!({}));
    let raw = RawResult::error(HANDLE_RESULT);
    let pending = vec![Pending {
        call: &call,
        raw: &raw,
    }];
    let outcome = bypassed_triage(&pending).expect("every result is bypassed");
    assert_eq!(outcome.results.len(), 1);
    assert!(matches!(outcome.results[0].wire, WireState::Raw));
    assert_eq!(outcome.results[0].origin, Origin::Fallback);
    assert!(
        outcome.results[0]
            .raw_keep
            .contains("fs-mcp write retry token"),
        "the breadcrumb names the shape, which is what separates a bypass from a \
         failed triage in the log: {}",
        outcome.results[0].raw_keep
    );
    // No request was made, so there is nothing to bill and no retry to record.
    assert!(outcome.usage.is_empty() && outcome.retries.is_empty());
}

/// An ordinary result is still the fork's: no bypass, and it is offered for a verdict.
#[test]
fn an_ordinary_result_still_goes_through_the_fork() {
    let call = ToolCall::new("read", "read_1", json!({}));
    let raw = RawResult::text("alpha\nbeta\n");
    let pending = vec![Pending {
        call: &call,
        raw: &raw,
    }];
    assert!(bypassed_triage(&pending).is_none());
    assert_eq!(offered_labels(&pending), vec!["r1".to_string()]);
}

/// A mixed triage still runs — the handle-bearing result is answered (an unpaired
/// `tool_use` makes the request invalid) but with a stand-in and no label, and only the
/// other result is offered, so the triage is never asked to decide the one the harness already decided.
#[test]
fn a_handle_bearing_result_is_answered_but_never_offered() {
    let calls = [
        ToolCall::new("write", "write_1", json!({})),
        ToolCall::new("read", "read_2", json!({})),
    ];
    let raws = [
        RawResult::error(HANDLE_RESULT),
        RawResult::text("alpha\nbeta\n"),
    ];
    let prefix = vec![
        entry(1, Payload::user("go")),
        entry(
            2,
            Payload::assistant(AssistantOutput::from_legacy_messages(
                vec![],
                vec![],
                calls.to_vec(),
            )),
        ),
    ];
    let pending: Vec<Pending<'_>> = calls
        .iter()
        .zip(&raws)
        .map(|(call, raw)| Pending { call, raw })
        .collect();

    assert!(
        bypassed_triage(&pending).is_none(),
        "one result still needs a verdict"
    );
    assert_eq!(labels(&pending), vec![None, Some("r1".to_string())]);
    assert_eq!(offered_labels(&pending), vec!["r1".to_string()]);

    let messages = wire(&triage_messages(
        &prefix,
        Reader::Main,
        &pending,
        &[],
        &[],
        &[],
    ));
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
    assert!(!blocks[0].1.contains("r1"), "{}", blocks[0].1);
    assert!(blocks[1].1.starts_with("═══ result r1 · read ═══"));
    assert!(blocks[1].1.contains("   1 │ alpha"));

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
        entry(1, Payload::user("go")),
        entry(
            2,
            Payload::assistant(AssistantOutput::from_legacy_messages(
                vec![],
                vec![],
                vec![ToolCall::new("write", "write_1", json!({}))],
            )),
        ),
        entry(
            3,
            Payload::tool_result("write_1", RawResult::error(HANDLE_RESULT), verdict.wire),
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

/// An `ask` reply is the operator's own words: the shape rule keeps it out of the
/// triage's offered results and delivers it raw, while the triage's request still answers
/// its `tool_use` (docs/process-monitor.md).
#[test]
fn an_ask_reply_is_bypassed_and_still_answered_in_the_triage() {
    let calls = vec![
        ToolCall::new(crate::log::ASK, "ask_1", json!({"question": "ok?"})),
        ToolCall::new("search", "search_2", json!({})),
    ];
    let prefix = vec![entry(
        1,
        Payload::assistant(AssistantOutput::from_legacy_messages(
            vec![],
            vec![],
            calls.clone(),
        )),
    )];
    let raws = [RawResult::text("do it"), RawResult::text("alpha\nbeta")];
    let pending: Vec<Pending<'_>> = calls
        .iter()
        .zip(&raws)
        .map(|(call, raw)| Pending { call, raw })
        .collect();

    assert_eq!(bypassed(&pending[0]), Some("operator reply"));
    assert_eq!(bypassed(&pending[1]), None);
    assert!(
        !reply_only_triage(&pending),
        "a search result is still the triage's"
    );
    assert_eq!(offered_labels(&pending), vec!["r1"]);

    let messages = wire(&triage_messages(
        &prefix,
        Reader::Main,
        &pending,
        &[],
        &[],
        &[],
    ));
    let tail = messages.last().unwrap();
    let views: Vec<(&str, &str)> = tail
        .content
        .iter()
        .filter_map(|b| match b {
            ApiBlock::ToolResult {
                tool_use_id,
                content,
                ..
            } => Some((tool_use_id.as_str(), content.as_text()?)),
            _ => None,
        })
        .collect();
    assert_eq!(views.len(), 2, "both calls answered");
    assert_eq!(views[0].0, "ask_1");
    assert!(
        views[0].1.contains("operator's reply") && !views[0].1.contains("do it"),
        "the stand-in names it and does not show the words: {}",
        views[0].1
    );
    assert!(views[1].1.starts_with("═══ result r1 · search ═══"));
}

/// A triage holding only ask replies has every verdict decided without a request, and
/// says so, so the loop knows it must triage the inbox itself.
#[test]
fn a_gate_of_only_ask_replies_needs_no_triage_and_delivers_them_raw() {
    let call = ToolCall::new(crate::log::ASK, "ask_1", json!({"question": "ok?"}));
    let raw = RawResult::text("yes");
    let pending = [Pending {
        call: &call,
        raw: &raw,
    }];
    assert!(reply_only_triage(&pending));
    let outcome = bypassed_triage(&pending).expect("decided without a request");
    assert_eq!(outcome.results.len(), 1);
    assert!(matches!(outcome.results[0].wire, WireState::Raw));
    assert!(
        outcome.results[0].raw_keep.contains("operator reply"),
        "{}",
        outcome.results[0].raw_keep
    );
    // A triage with no ask in it is not this case, whatever else was bypassed.
    let other = ToolCall::new("search", "s", json!({}));
    assert!(!reply_only_triage(&[Pending {
        call: &other,
        raw: &raw
    }]));
}
