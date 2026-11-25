//! Tests for the thinking-off recovery (docs/transport.md): a 400 naming the
//! thinking type the request sent is retried once with adaptive thinking at effort
//! `low`, and the client sends that form from then on.

use super::forced_choice::{STREAM, last_user_text, scripted};
use super::*;
use crate::record::log::RequestOrigin;
use crate::record::project::WireTurn;

/// The refusal Opus 5.5 and Fable 5.x answer `thinking: between_tools` with
/// (docs/transport.md): a 400 naming the thinking type, which is what the recovery
/// matches — not an arbitrary 400.
const REFUSAL: &str = r#"{"type":"error","error":{"type":"invalid_request_error","message":"thinking.type: between_tools is not supported"}}"#;

/// A 400 about something else entirely — a malformed request, naming neither the
/// thinking type nor the output cap, so no other recovery matches it either: no
/// thinking fallback, nothing learned.
const OTHER: &str = r#"{"type":"error","error":{"type":"invalid_request_error","message":"messages.0.content.0.text: field required"}}"#;

/// A request that spells thinking off is retried at adaptive `low`, and the second
/// attempt's bytes are what every later request sends: one refusal, then no more.
#[tokio::test]
async fn a_thinking_off_refusal_is_retried_adaptive_at_low_and_remembered() {
    let (client, server) = scripted(vec![(400, REFUSAL), (200, STREAM), (200, STREAM)]);
    let client = client.without_thinking();
    let turns = [WireTurn::text("is this safe?")];
    client
        .complete(&turns, &[], ToolChoice::auto(), |_| {})
        .await
        .unwrap();
    client
        .complete(&turns, &[], ToolChoice::auto(), |_| {})
        .await
        .unwrap();
    let bodies = server.bodies();

    assert_eq!(bodies[0]["thinking"]["type"], "between_tools");
    assert!(bodies[0].get("output_config").is_none(), "{:?}", bodies[0]);
    assert_eq!(bodies[1]["thinking"]["type"], "adaptive");
    assert_eq!(bodies[1]["output_config"]["effort"], "low");
    assert_eq!(
        bodies[1]["messages"], bodies[0]["messages"],
        "the turns are what the request sent"
    );
    assert_eq!(
        bodies[2], bodies[1],
        "the recovery is reused: the later request is one send, not a refusal and a retry"
    );
}

/// A forced call cannot carry thinking on, so its retry goes as `auto` with the
/// instruction naming the tool — and that is the form every later forced call takes,
/// without paying the refusal again.
#[tokio::test]
async fn a_forced_call_refused_for_thinking_is_retried_as_auto_with_the_instruction() {
    let (client, server) = scripted(vec![(400, REFUSAL), (200, STREAM), (200, STREAM)]);
    let turns = [WireTurn::text("is this safe?")];
    let forced = ToolChoice::force("spot");
    client.complete(&turns, &[], forced, |_| {}).await.unwrap();
    client.complete(&turns, &[], forced, |_| {}).await.unwrap();
    let bodies = server.bodies();

    assert_eq!(bodies[0]["tool_choice"]["type"], "tool");
    assert_eq!(bodies[0]["thinking"]["type"], "between_tools");
    for body in &bodies[1..] {
        assert_eq!(body["tool_choice"]["type"], "auto", "{body}");
        assert_eq!(body["thinking"]["type"], "adaptive", "{body}");
        assert_eq!(body["output_config"]["effort"], "low", "{body}");
        assert!(
            last_user_text(body).contains("calling the `spot` tool"),
            "{body}"
        );
    }
}

/// The instruction appends a turn, so the forced retry is off the cache chain, as the
/// forced-choice fallback is: its response is not the body the next turn extends.
#[tokio::test]
async fn the_forced_thinking_retry_stays_off_the_cache_chain() {
    let (client, server) = scripted(vec![(400, REFUSAL), (200, STREAM)]);
    let client = client.with_origin(RequestOrigin::Turn);
    let turns = [WireTurn::text("is this safe?")];
    client
        .complete(&turns, &[], ToolChoice::force("spot"), |_| {})
        .await
        .unwrap();
    let bodies = server.bodies();

    assert_eq!(
        bodies[0]["diagnostics"]["previous_message_id"],
        Value::Null,
        "the request that was refused opts in: {}",
        bodies[0]
    );
    assert!(
        bodies[1].get("diagnostics").is_none(),
        "the retry appended a turn, so it is off the chain: {}",
        bodies[1]
    );
}

/// An `auto` retry sends the turns the request did, so it stays on the chain: only
/// the envelope's thinking and effort moved.
#[tokio::test]
async fn the_auto_thinking_retry_stays_on_the_cache_chain() {
    let (client, server) = scripted(vec![(400, REFUSAL), (200, STREAM)]);
    let client = client.without_thinking().with_origin(RequestOrigin::Turn);
    let turns = [WireTurn::text("is this safe?")];
    client
        .complete(&turns, &[], ToolChoice::auto(), |_| {})
        .await
        .unwrap();
    let bodies = server.bodies();

    assert_eq!(bodies[0]["diagnostics"]["previous_message_id"], Value::Null);
    assert_eq!(
        bodies[1]["diagnostics"], bodies[0]["diagnostics"],
        "the turns are unchanged, so the retry is still this turn's body: {}",
        bodies[1]
    );
}

/// The recovery is learned once and applies to the client's later calls of any shape:
/// a forced call after an ordinary one goes straight to `auto` with the instruction,
/// one request and no refusal.
#[tokio::test]
async fn a_later_forced_call_goes_straight_to_the_recovered_form() {
    let (client, server) = scripted(vec![(400, REFUSAL), (200, STREAM), (200, STREAM)]);
    let client = client.without_thinking();
    let turns = [WireTurn::text("is this safe?")];
    let forced = ToolChoice::force("spot");
    client
        .complete(&turns, &[], ToolChoice::auto(), |_| {})
        .await
        .unwrap();
    client.complete(&turns, &[], forced, |_| {}).await.unwrap();
    let bodies = server.bodies();

    assert_eq!(bodies[0]["thinking"]["type"], "between_tools");
    assert_eq!(bodies[1]["thinking"]["type"], "adaptive");
    assert_eq!(bodies[2]["tool_choice"]["type"], "auto");
    assert_eq!(bodies[2]["thinking"]["type"], "adaptive");
    assert_eq!(bodies[2]["output_config"]["effort"], "low");
    assert!(last_user_text(&bodies[2]).contains("calling the `spot` tool"));
}

/// A 400 about anything else is not retried and teaches nothing: the next request
/// still spells thinking off.
#[tokio::test]
async fn an_unrelated_400_gets_no_thinking_fallback() {
    let (client, server) = scripted(vec![(400, OTHER), (200, STREAM)]);
    let client = client.without_thinking();
    let turns = [WireTurn::text("is this safe?")];
    let error = client
        .complete(&turns, &[], ToolChoice::auto(), |_| {})
        .await
        .unwrap_err();
    assert!(error.to_string().contains("field required"), "{error}");
    client
        .complete(&turns, &[], ToolChoice::auto(), |_| {})
        .await
        .unwrap();
    let bodies = server.bodies();

    assert_eq!(bodies[1]["thinking"]["type"], "between_tools", "unlearned");
}

/// When the retry is refused too, the rejection had another cause: the first error
/// comes back and nothing is learned.
#[tokio::test]
async fn a_recovery_the_retry_does_not_cure_is_returned_and_not_learned() {
    let (client, server) = scripted(vec![(400, REFUSAL), (400, OTHER), (200, STREAM)]);
    let client = client.without_thinking();
    let turns = [WireTurn::text("is this safe?")];
    let error = client
        .complete(&turns, &[], ToolChoice::auto(), |_| {})
        .await
        .unwrap_err();
    assert!(error.to_string().contains("between_tools"), "{error}");
    client
        .complete(&turns, &[], ToolChoice::auto(), |_| {})
        .await
        .unwrap();
    let bodies = server.bodies();

    assert_eq!(bodies[2]["thinking"]["type"], "between_tools", "unlearned");
}

/// The recovery needs a model that takes adaptive thinking: without it there is no
/// form to fall back to, so the refusal is the answer.
#[tokio::test]
async fn a_model_without_adaptive_thinking_gets_no_fallback() {
    let (client, server) = scripted(vec![(400, REFUSAL)]);
    let client = client.with_thinking(ThinkingSupport {
        adaptive: false,
        off: ThinkingOff::BetweenTools,
    });
    let turns = [WireTurn::text("is this safe?")];
    assert!(
        client
            .complete(&turns, &[], ToolChoice::auto(), |_| {})
            .await
            .is_err()
    );
    assert_eq!(server.bodies().len(), 1);
}

/// A separate-model result triage answers the refusal with its own off → `low` →
/// unset ladder, so the recovery stays out of the way: the ladder records its step
/// and the request goes as the ladder's `low`, not as the recovery's.
#[tokio::test]
async fn the_separate_triage_ladder_still_answers_the_refusal() {
    let (client, server) = scripted(vec![(400, REFUSAL), (200, STREAM)]);
    let turns = [WireTurn::text("retain useful lines")];
    let mut retries = Vec::new();
    let (answer, mode) = client
        .complete_separate_triage(
            &turns,
            &[],
            ToolChoice::force("retain"),
            |_| {},
            &mut retries,
        )
        .await;
    answer.unwrap();

    assert_eq!(mode, TriageReasoning::Low);
    assert_eq!(retries.len(), 1, "{retries:?}");
    let bodies = server.bodies();
    assert_eq!(bodies[0]["thinking"]["type"], "between_tools");
    assert!(
        bodies[1].get("thinking").is_none(),
        "the ladder's `low` step sends no thinking field under a forced choice: {}",
        bodies[1]
    );
    assert_eq!(bodies[1]["output_config"]["effort"], "low");
}
