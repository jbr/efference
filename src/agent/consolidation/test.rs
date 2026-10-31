use super::*;
use crate::log::{AssistantOutput, Provider, ToolCall};

fn response(text: &str) -> AssistantOutput {
    AssistantOutput::Messages(vec![crate::log::MessagesOutputBlock::Text(text.into())])
}

#[test]
fn rejects_skip_short_tools_failures_and_caps() {
    assert_eq!(
        account(&response(" SKIP \n"), None, None).unwrap_err(),
        "skip"
    );
    assert_eq!(
        account(&response(""), None, None).unwrap_err(),
        "short account"
    );
    assert_eq!(
        account(&response("not enough"), None, None).unwrap_err(),
        "short account"
    );
    let text = "supported ".repeat(100);
    assert_eq!(account(&response(&text), None, None).unwrap(), text.trim());
    let capped = crate::api::Finish {
        reason: FinishReason::OutputCap,
        raw: "max_tokens".into(),
    };
    assert!(account(&response(&text), Some(&capped), None).is_err());
    assert!(account(&response(&text), None, Some("interrupted")).is_err());
    let mut called = response(&text);
    if let AssistantOutput::Messages(blocks) = &mut called {
        blocks.push(crate::log::MessagesOutputBlock::ToolUse(ToolCall::new(
            "read",
            "call",
            serde_json::json!({}),
        )));
    }
    assert_eq!(account(&called, None, None).unwrap_err(), "tool call");
}

use crate::log::{RawResult, Summary, WireState};
use crate::testing::upstream::{Canned, ScriptedUpstream};
use std::sync::Arc;

fn session() -> Arc<Session> {
    let (shared, _) = Session::in_memory();
    shared
        .append(0, Payload::user("inspect this source"))
        .unwrap();
    shared
        .append(
            0,
            Payload::assistant(AssistantOutput::Messages(vec![
                crate::log::MessagesOutputBlock::ToolUse(ToolCall::new(
                    "read",
                    "r",
                    serde_json::json!({"path":"source.rs"}),
                )),
            ])),
        )
        .unwrap();
    shared
        .append(
            0,
            Payload::tool_result(
                "r",
                RawResult::text("evidence ".repeat(9000)),
                WireState::Raw,
            ),
        )
        .unwrap();
    shared
        .append(
            0,
            Payload::assistant(response("the investigation is complete")),
        )
        .unwrap();
    shared
}

/// An upstream answering every consolidation request with an account whose text is
/// `text`, after a line of thinking, or with a 400 when `fail`; and a client against it.
fn mock(text: &str, fail: bool) -> (ScriptedUpstream, Client) {
    let body = format!(
        "event: message_start\ndata: {{\"type\":\"message_start\",\"message\":{{\"id\":\"m\",\"model\":\"m\",\"usage\":{{\"input_tokens\":1,\"output_tokens\":0}}}}}}\n\nevent: content_block_start\ndata: {{\"type\":\"content_block_start\",\"index\":0,\"content_block\":{{\"type\":\"thinking\",\"thinking\":\"\"}}}}\n\nevent: content_block_delta\ndata: {{\"type\":\"content_block_delta\",\"index\":0,\"delta\":{{\"type\":\"thinking_delta\",\"thinking\":\"reviewing\"}}}}\n\nevent: content_block_stop\ndata: {{\"type\":\"content_block_stop\",\"index\":0}}\n\nevent: content_block_start\ndata: {{\"type\":\"content_block_start\",\"index\":1,\"content_block\":{{\"type\":\"text\",\"text\":\"\"}}}}\n\nevent: content_block_delta\ndata: {{\"type\":\"content_block_delta\",\"index\":1,\"delta\":{{\"type\":\"text_delta\",\"text\":{}}}}}\n\nevent: content_block_stop\ndata: {{\"type\":\"content_block_stop\",\"index\":1}}\n\nevent: message_delta\ndata: {{\"type\":\"message_delta\",\"delta\":{{\"stop_reason\":\"end_turn\"}},\"usage\":{{\"output_tokens\":100}}}}\n\nevent: message_stop\ndata: {{\"type\":\"message_stop\"}}\n\n",
        serde_json::to_string(text).unwrap()
    );
    let upstream = ScriptedUpstream::new(move |_| {
        if fail {
            Canned::json(400, "bad").without_content_type()
        } else {
            Canned::stream(body.clone())
        }
    });
    let client = Client::new(
        upstream.endpoint(Provider::Anthropic, "/v1/messages"),
        "m",
        "sys",
        None,
    );
    (upstream, client)
}

#[tokio::test]
async fn unchanged_skip_not_rechecked_but_new_forward_context_is() {
    let shared = session();
    let (upstream, client) = mock("skip", false);
    let check = ConsolidationCheck::new(&client, &[]);
    if let Some(task) = check.check(&shared) {
        task.await.unwrap();
    }
    if let Some(task) = check.check(&shared) {
        task.await.unwrap();
    }
    assert_eq!(upstream.count(), 1);
    assert!(
        !shared
            .snapshot()
            .iter()
            .any(|e| matches!(e.payload, Payload::Summary(_)))
    );
    shared.append(1, Payload::user("next task")).unwrap();
    if let Some(task) = check.check(&shared) {
        task.await.unwrap();
    }
    assert_eq!(upstream.count(), 2);
}

#[tokio::test]
async fn a_failed_request_changes_nothing() {
    let shared = session();
    let (_upstream, client) = mock("", true);
    let check = ConsolidationCheck::new(&client, &[]);
    if let Some(task) = check.check(&shared) {
        task.await.unwrap();
    }
    assert!(shared.snapshot().iter().any(|e| matches!(
        &e.payload,
        Payload::Telemetry(Telemetry::Consolidation { outcome, .. })
            if outcome.starts_with("request error")
    )));
    assert!(
        !shared
            .snapshot()
            .iter()
            .any(|e| matches!(e.payload, Payload::Summary(_)))
    );
}

#[tokio::test]
async fn adopts_account_then_waits_for_new_tool_work() {
    let shared = session();
    let (upstream, client) = mock(&"supported outcome ".repeat(60), false);
    let check = ConsolidationCheck::new(&client, &[]);
    let mut output = shared.subscribe_stream();
    let before = shared.project_turns();
    if let Some(task) = check.check(&shared) {
        task.await.unwrap();
    }
    let mut streamed = String::new();
    let mut thinking = String::new();
    while let Ok(delta) = output.try_recv() {
        match delta {
            StreamDelta::ConsolidationText { text } => streamed.push_str(&text),
            StreamDelta::ConsolidationThinking { text } => thinking.push_str(&text),
            _ => panic!("unexpected stream kind"),
        }
    }
    assert_eq!(streamed, "supported outcome ".repeat(60));
    assert_eq!(thinking, "reviewing");
    let after = shared.project_turns();
    assert_eq!(after.last(), before.last());
    assert!(after.len() < before.len());
    assert!(
        shared
            .snapshot()
            .iter()
            .any(|e| matches!(e.payload, Payload::Summary(_)))
    );
    shared.append(1, Payload::user("new task")).unwrap();
    if let Some(task) = check.check(&shared) {
        task.await.unwrap();
    }
    assert_eq!(upstream.count(), 1);
}

#[test]
fn frozen_adoption_preserves_new_messages_and_rejects_new_folds() {
    let shared = session();
    let frozen = shared.snapshot();
    let span = consolidate::candidate(&frozen).unwrap();
    shared.append(1, Payload::user("new direction")).unwrap();
    assert!(
        shared
            .adopt_consolidation(&frozen, span, "account".into())
            .unwrap()
    );
    assert_eq!(
        shared.project_turns().last(),
        Some(&WireTurn::text("new direction"))
    );
    let shared = session();
    let frozen = shared.snapshot();
    let span = consolidate::candidate(&frozen).unwrap();
    shared
        .append(
            1,
            Payload::Summary(Summary::consolidation(0, span.upto, "head summary")),
        )
        .unwrap();
    assert!(
        !shared
            .adopt_consolidation(&frozen, span, "account".into())
            .unwrap()
    );
}

#[test]
fn adoption_tolerates_concurrent_work_but_not_rewrites_of_the_range() {
    let shared = session();
    let frozen = shared.snapshot();
    let span = consolidate::candidate(&frozen).unwrap();
    // Work the trajectory did while the account was being written: a call still
    // waiting for its result, and a verdict on a result after the snapshot.
    shared.append(1, Payload::user("go on")).unwrap();
    shared
        .append(
            1,
            Payload::assistant(AssistantOutput::Messages(vec![
                crate::log::MessagesOutputBlock::ToolUse(ToolCall::new(
                    "read",
                    "later",
                    serde_json::json!({"path":"other.rs"}),
                )),
            ])),
        )
        .unwrap();
    let later = shared
        .append(
            1,
            Payload::tool_result("later", RawResult::text("other"), WireState::Raw),
        )
        .unwrap();
    shared
        .append(1, Payload::result_verdict(later, WireState::Elided))
        .unwrap();
    assert!(
        shared
            .adopt_consolidation(&frozen, span, "account".into())
            .unwrap()
    );

    // A verdict on a result inside the frozen snapshot changes what the account
    // was written from.
    let shared = session();
    let frozen = shared.snapshot();
    let span = consolidate::candidate(&frozen).unwrap();
    shared
        .append(1, Payload::result_verdict(span.upto, WireState::Elided))
        .unwrap();
    assert!(
        !shared
            .adopt_consolidation(&frozen, span, "account".into())
            .unwrap()
    );

    // A `consolidate` call made meanwhile computes its own range without this one.
    let shared = session();
    let frozen = shared.snapshot();
    let span = consolidate::candidate(&frozen).unwrap();
    shared
        .append(
            1,
            Payload::assistant(AssistantOutput::Messages(vec![
                crate::log::MessagesOutputBlock::ToolUse(ToolCall::new(
                    consolidate::NAME,
                    "manual",
                    serde_json::json!({"summary":"s"}),
                )),
            ])),
        )
        .unwrap();
    assert!(
        !shared
            .adopt_consolidation(&frozen, span, "account".into())
            .unwrap()
    );
}

#[test]
fn instruction_names_what_follows_the_range() {
    let shared = session();
    let entries = shared.snapshot();
    let span = consolidate::candidate(&entries).unwrap();
    assert!(instruction(&entries, span).contains("the report or question that follows it"));

    // An operator message that arrived mid-work, with no report before it,
    // anchors the range instead.
    let (shared, _) = Session::in_memory();
    for payload in entries
        .iter()
        .take_while(|e| e.seq <= span.upto)
        .map(|e| e.payload.clone())
        .chain([Payload::user("actually, the ui")])
    {
        shared.append(0, payload).unwrap();
    }
    let interjected = shared.snapshot();
    let span = consolidate::candidate(&interjected).unwrap();
    let text = instruction(&interjected, span);
    assert!(text.contains("arrived while this work was under way"));
}

#[tokio::test]
async fn a_juncture_reached_during_a_check_is_not_checked() {
    let shared = session();
    let (upstream, client) = mock("skip", false);
    let check = ConsolidationCheck::new(&client, &[]);
    check.flight.lock().unwrap().running = true;
    assert!(check.check(&shared).is_none());
    assert_eq!(upstream.count(), 0);
}
