//! Tests for what gets rejected and what retries.
use super::*;

/// The in-fork retry (docs/triage.md) must keep the fork conversation
/// API-valid: replay the non-compliant assistant turn, answer each of its calls with an
/// error `tool_result`, and renew the ask — so the retry extends the warm prefix
/// instead of opening a new request shape.
#[test]
fn a_retry_tail_answers_every_wrong_call_and_renews_the_ask() {
    let completion = retain_completion("bash", json!({"command": "ls"}));
    let mut messages = vec![WireTurn::text("the triage tail")];
    let why = unusable(&completion, RETAIN);
    assert!(extend_with_retry(
        &mut messages,
        &completion.assistant,
        &why,
        &call_now(RETAIN)
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
                content.contains("not accepted — triage called bash instead of retain"),
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
        receipt: Default::default(),
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
        finish: None,
        usage: Usage {
            input_tokens: 0,
            output_tokens: 0,
            cache_creation_input_tokens: 0,
            cache_read_input_tokens: 0,
        },
        cache_miss: None,
        failure_class: crate::api::FailureClass::Transient,
    };
    let mut messages = Vec::new();
    assert!(extend_with_retry(
        &mut messages,
        &completion.assistant,
        "triage called no tool",
        &call_now(TRIAGE)
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
        "triage called no tool",
        &call_now(RETAIN)
    ));
    assert!(messages.is_empty());
}

/// Coverage is part of verdict validity (docs/triage.md): a verdict that
/// leaves any offered result unanswered is rejected — naming the missing ids so the
/// retry nudge is actionable — rather than letting the result fall through untriaged.
#[test]
fn an_incomplete_verdict_is_rejected_naming_the_missing_ids() {
    let offered = ["a".to_string(), "b".to_string()];
    let complete = parse_retain(
        &retain_completion(
            RETAIN,
            json!({ "results": [
                { "result": "a", "keep": "all" },
                { "result": "b", "keep": "none", "note": "n" },
            ] }),
        ),
        &offered,
    )
    .unwrap();
    assert!(require_coverage(complete, &offered).is_ok());

    let partial = parse_retain(
        &retain_completion(
            RETAIN,
            json!({ "results": [{ "result": "a", "keep": "all" }] }),
        ),
        &offered,
    )
    .unwrap();
    let Err(why) = require_coverage(partial, &offered) else {
        panic!("a partial verdict must be rejected");
    };
    assert!(why.contains("[b]"), "names the unanswered id: {why}");
}

/// Bounds are part of verdict validity (docs/triage.md): a `keep` range wholly
/// beyond the shown line count is a coordinate mistake — the shape it catches is a triage
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

    // Overshoot from an in-bounds start is the triage's line accounting running a line
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
                { "result": "a", "keep": "all" },
                { "result": "b", "keep": "none" },
                { "result": "ghost", "keep": "900-901" },
            ] }),
        ),
        &offered,
    )
    .unwrap();
    assert!(require_in_bounds(p, &shown).is_ok());
}

/// A triage the operator interrupted before a verdict (docs/control-channel.md) withholds
/// every result as the interrupt's *after* juncture does — `Elided`, never an error that
/// blames triage, never the untriaged raw — and keeps the held attempts' breadcrumbs.
#[test]
fn an_interrupted_gate_withholds_every_result_without_blaming_triage() {
    let outcome = HoldOutcome::interrupted(2, vec!["held: overloaded".into()], Vec::new());
    assert_eq!(outcome.results.len(), 2);
    for d in &outcome.results {
        assert!(matches!(d.wire, WireState::Elided), "withheld, not kept");
        assert_eq!(d.origin, Origin::Fallback);
    }
    assert_eq!(outcome.retries, ["held: overloaded"]);
    assert!(outcome.inbox.is_empty() && outcome.background.is_empty());
}

/// The coverage rejection names what the verdict *did* answer, not only what it missed.
/// "Answered nothing" and "answered a pile of ids that were never offered" are different
/// defects — the second is a prompt-scoping failure — and the breadcrumb is the only
/// place a later reader can tell them apart (docs/triage.md).
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
                { "result": "6", "keep": "all" },
                { "result": "multi_tool_use.parallel", "keep": "all" },
                { "result": "8", "keep": "all" },
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

/// The triage retries a stream that dies before finalizing on the **same budget and
/// schedule** as the main loop's own aborted turns (docs/transport.md) — one policy for
/// one failure, whichever surface it lands on. Before this, an aborted triage withheld on
/// the first abort while an aborted turn got four attempts; a controlled run caught the
/// asymmetry costing nine turns and a spurious conformance failure (docs/transport.md).
///
/// The cap is the part worth pinning: it is what lets the attempt count be large enough
/// to wait a flaky provider out without the tail of the schedule becoming a hang.
#[test]
fn an_aborted_triage_stream_backs_off_on_the_main_loops_budget() {
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

/// Parallel calls are allowed on the shared envelope (docs/README.md), so a triage may
/// answer a two-result triage in two `retain` calls; the verdicts merge, and the merged
/// verdict covers the triage.
#[test]
fn a_verdict_split_across_parallel_retain_calls_merges() {
    let offered = ["a".to_string(), "b".to_string()];
    let split = parse_retain(
        &calls_completion(vec![
            ToolCall::new(
                RETAIN,
                "r1",
                json!({ "results": [{ "result": "a", "keep": "all" }], "surfaced": ["x"] }),
            ),
            ToolCall::new(
                RETAIN,
                "r2",
                json!({ "results": [{ "result": "b", "keep": "none", "note": "n" }] }),
            ),
        ]),
        &offered,
    )
    .unwrap();
    assert_eq!(split.surfaced, ["x"]);
    assert!(require_coverage(split, &offered).is_ok());
}
