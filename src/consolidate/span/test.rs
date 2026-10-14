use super::*;
use crate::log::MessagesOutputBlock;
use crate::log::{
    AssistantOutput, ContentBlock, Distilled, Mark, RawResult, Timestamp, ToolCall, WireState,
};
use serde_json::json;

fn entry(seq: Seq, payload: Payload) -> LogEntry {
    LogEntry {
        seq,
        turn: 0,
        at: Timestamp(seq),
        submission_id: None,
        source: None,
        payload,
    }
}
fn user(seq: Seq, text: &str) -> LogEntry {
    entry(seq, Payload::User(vec![ContentBlock::Text(text.into())]))
}
fn call(seq: Seq, id: &str, name: &str) -> LogEntry {
    entry(
        seq,
        Payload::Assistant {
            output: AssistantOutput::Messages(vec![MessagesOutputBlock::ToolUse(ToolCall::new(
                name,
                id,
                json!({"path":"source.rs"}),
            ))]),
            upstream_failure: None,
        },
    )
}
fn result(seq: Seq, id: &str, text: &str) -> LogEntry {
    entry(
        seq,
        Payload::tool_result(id, RawResult::text(text), WireState::Raw),
    )
}
fn report(seq: Seq, text: &str) -> LogEntry {
    entry(
        seq,
        Payload::Assistant {
            output: AssistantOutput::Messages(vec![MessagesOutputBlock::Text(text.into())]),
            upstream_failure: None,
        },
    )
}
fn work() -> Vec<LogEntry> {
    vec![
        user(0, "instruction"),
        call(1, "r", "read"),
        result(2, "r", "retained result with substantial extra raw content"),
        report(3, "completed report"),
        user(4, "new direction"),
    ]
}

#[test]
fn preserves_final_report_and_operator_reply() {
    let mut log = work();
    let span = candidate(&log).unwrap();
    assert_eq!(span, Span { from: 0, upto: 2 });
    log.push(entry(
        5,
        Payload::Mark(Mark::span(span.from, span.upto, "account")),
    ));
    let wire = project_semantic(&log);
    assert_eq!(
        wire,
        vec![
            WireTurn::text(
                "⟢ consolidation: entries #0–#2 were replaced by this account of them, \
                 written by this session with those entries in view.\n\naccount"
            ),
            WireTurn::Assistant(AssistantOutput::Messages(vec![MessagesOutputBlock::Text(
                "completed report".into()
            )])),
            WireTurn::text("new direction")
        ]
    );
    assert!(candidate(&log).is_none());
}

#[test]
fn later_span_does_not_rewrite_prefix_or_earlier_account() {
    let mut log = work();
    log.push(entry(5, Payload::Mark(Mark::span(0, 2, "account A"))));
    log.extend([
        call(6, "edit", "edit"),
        result(7, "edit", "edited"),
        report(8, "report B"),
    ]);
    let prefix = project_semantic(&log);
    let span = candidate(&log).unwrap();
    assert_eq!(span, Span { from: 3, upto: 7 });
    log.push(entry(
        9,
        Payload::Mark(Mark::span(span.from, span.upto, "account B")),
    ));
    let wire = project_semantic(&log);
    assert_eq!(wire[0], prefix[0]);
    assert_eq!(wire[0], account_turn(0, 2, "account A"));
    assert_eq!(wire[1], account_turn(3, 7, "account B"));
    assert_eq!(wire.last(), prefix.last());
}

#[test]
fn rejects_pending_and_split_batches_even_outside_span() {
    let mut log = work();
    log.push(call(5, "pending", "read"));
    assert!(candidate(&log).is_none());
    assert!(!safe(&log[..4], Span { from: 0, upto: 1 }));
    assert!(!safe(&log[..4], Span { from: 2, upto: 2 }));
    log.push(result(6, "pending", "paired"));
    assert!(safe(&log, Span { from: 0, upto: 2 }));
}

#[test]
fn ask_waits_for_all_sibling_results_and_keeps_question_reply() {
    let mut log = work()[..3].to_vec();
    log.push(entry(
        3,
        Payload::Assistant {
            output: AssistantOutput::Messages(vec![
                MessagesOutputBlock::ToolUse(ToolCall::new(
                    "ask",
                    "q",
                    json!({"question":"next?"}),
                )),
                MessagesOutputBlock::ToolUse(ToolCall::new("read", "sibling", json!({}))),
            ]),
            upstream_failure: None,
        },
    ));
    assert!(candidate(&log).is_none());
    log.push(result(4, "q", "answer"));
    assert!(candidate(&log).is_none());
    log.push(result(5, "sibling", "complete"));
    assert_eq!(candidate(&log), Some(Span { from: 0, upto: 2 }));
}

#[test]
fn budget_counts_projected_verdict_text_and_json_not_raw_output() {
    let mut log = work();
    let span = candidate(&log).unwrap();
    let arguments = serde_json::to_vec(&json!({"path":"source.rs"}))
        .unwrap()
        .len();
    assert_eq!(
        tool_bytes(&log, span),
        arguments + "[2] retained result with substantial extra raw content".len()
    );
    log[2] = entry(
        2,
        Payload::tool_result(
            "r",
            RawResult::text("retained result with substantial extra raw content"),
            WireState::Pending,
        ),
    );
    assert_eq!(tool_bytes(&log, span), 0);
    log.push(entry(
        5,
        Payload::ResultVerdict {
            for_result: 2,
            wire: WireState::Distilled(Distilled::new(crate::log::Keep::None).with_note("short")),
        },
    ));
    let projected = project_semantic(&log);
    let text_len = projected
        .iter()
        .filter_map(|t| match t {
            WireTurn::User(parts) => Some(parts),
            _ => None,
        })
        .flatten()
        .find_map(|p| match p {
            UserPart::ToolResult { content, .. } => Some(content.len()),
            _ => None,
        })
        .unwrap();
    assert_eq!(tool_bytes(&log, span), arguments + text_len);
    assert!(!format!("{projected:?}").contains("substantial extra raw content"));
}

#[test]
fn manual_consolidation_is_not_tool_work() {
    let log = vec![
        user(0, "work"),
        call(1, "c", NAME),
        result(2, "c", "consolidated"),
        report(3, "done"),
    ];
    assert_eq!(tool_bytes(&log, Span { from: 0, upto: 2 }), 0);
}

fn account_turn(from: Seq, upto: Seq, note: &str) -> WireTurn {
    WireTurn::text(format!(
        "⟢ consolidation: entries #{from}–#{upto} were replaced by this account of them, \
         written by this session with those entries in view.\n\n{note}"
    ))
}
