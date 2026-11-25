//! Tests for the thinking-off recovery (docs/transport.md): a 400 naming the
//! thinking type the request sent is retried once with adaptive thinking at effort
//! `low`, and the client sends that form from then on.

use super::forced_choice::{STREAM, last_user_text, scripted};
use super::images::{carries_image, with_image};
use super::*;
use crate::inference::testing::upstream::{Canned, Request, ScriptedUpstream};
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

/// A 400 the adaptive retry meets instead: the level refused, not the thinking type.
/// The recovery returns the first refusal, not this one.
const EFFORT_REFUSAL: &str = r#"{"type":"error","error":{"type":"invalid_request_error","message":"output_config.effort: unsupported level"}}"#;

/// A stream's opening event and nothing after it: left to end, the read assembles a
/// partial; held open, it is the read an interrupt ends.
const FIRST_EVENT: &str = "event: message_start\ndata: {\"message\":{\"id\":\"msg-1\",\
     \"model\":\"m\",\"usage\":{\"input_tokens\":0,\"output_tokens\":0}}}\n\n";

/// The bodies of every request `upstream` was sent, as JSON, in arrival order.
fn request_bodies(upstream: &ScriptedUpstream) -> Vec<Value> {
    upstream.requests().iter().map(Request::json).collect()
}

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

/// The recovery's one retry is the only one. When it fails, the refusal comes back
/// marked as the recovery's, so `complete` does not read it as a forced-choice
/// refusal and send `auto` with thinking off — which would meet the same refusal a
/// second time. Two requests, and the first error returned.
#[tokio::test]
async fn a_failed_forced_recovery_is_not_answered_again_by_the_forced_fallback() {
    let (client, server) = scripted(vec![(400, REFUSAL), (400, EFFORT_REFUSAL)]);
    let turns = [WireTurn::text("is this safe?")];
    let error = client
        .complete(&turns, &[], ToolChoice::force("spot"), |_| {})
        .await
        .unwrap_err();

    assert!(error.to_string().contains("between_tools"), "{error}");
    assert!(
        !error.to_string().contains("unsupported level"),
        "the first error is returned, not the retry's: {error}"
    );
    let bodies = server.bodies();
    assert_eq!(bodies.len(), 2, "the recovery's retry is the only one");
    assert_eq!(bodies[1]["tool_choice"]["type"], "auto", "{}", bodies[1]);
    assert_eq!(bodies[1]["thinking"]["type"], "adaptive", "{}", bodies[1]);
    assert_eq!(bodies[1]["output_config"]["effort"], "low", "{}", bodies[1]);
}

/// The same ownership over the image recovery: a request carrying an image whose
/// thinking-off spelling is refused is retried with thinking on and the image still
/// in it, and when that fails the image recovery does not strip the images and send
/// the refused spelling again. Two requests, and no lesson about images.
#[tokio::test]
async fn an_image_bearing_thinking_refusal_is_not_answered_again_by_the_image_recovery() {
    let (client, server) = scripted(vec![(400, REFUSAL), (400, EFFORT_REFUSAL)]);
    let client = client.without_thinking();
    let turns = with_image();
    let error = client
        .complete(&turns, &[], ToolChoice::auto(), |_| {})
        .await
        .unwrap_err();

    assert!(error.to_string().contains("between_tools"), "{error}");
    assert_eq!(
        client.images_refused(),
        None,
        "the image recovery learned nothing"
    );
    let bodies = server.bodies();
    assert_eq!(bodies.len(), 2, "the recovery's retry is the only one");
    assert!(
        carries_image(&bodies[1]),
        "the retry still carries the image: {}",
        bodies[1]
    );
}

/// A retry whose stream dies before finalizing is not a cured refusal: the partial
/// comes back and nothing is learned, so the next request meets the refusal again and
/// recovers.
#[tokio::test]
async fn a_stream_failure_on_the_retry_learns_nothing() {
    let (client, server) = scripted(vec![
        (400, REFUSAL),
        (200, FIRST_EVENT),
        (400, REFUSAL),
        (200, STREAM),
    ]);
    let client = client.without_thinking();
    let turns = [WireTurn::text("is this safe?")];
    let completion = client
        .complete(&turns, &[], ToolChoice::auto(), |_| {})
        .await
        .unwrap();

    assert!(
        completion.upstream_failure().is_some(),
        "the retry's stream ended without finalizing"
    );
    client
        .complete(&turns, &[], ToolChoice::auto(), |_| {})
        .await
        .unwrap();
    let bodies = server.bodies();
    assert_eq!(bodies[2]["thinking"]["type"], "between_tools", "unlearned");
    assert_eq!(bodies[3]["thinking"]["type"], "adaptive");
    assert_eq!(bodies[3]["output_config"]["effort"], "low");
}

/// The interrupt the retry reads against is the one `send` armed: a press during the
/// recovery ends the retry as an interrupt, which comes back as one — not as the
/// refusal — and teaches nothing, so the next call meets the refusal again.
#[tokio::test(start_paused = true)]
async fn an_interrupt_during_the_recovery_stays_an_interrupt() {
    let upstream = ScriptedUpstream::answering([
        Canned::json(400, REFUSAL),
        Canned::stream(FIRST_EVENT).left_open(),
        Canned::json(400, REFUSAL),
        Canned::stream(STREAM),
    ]);
    let interrupt = std::sync::Arc::new(tokio::sync::Notify::new());
    let mut client = Client::new(
        upstream.endpoint(Provider::Anthropic, "/v1/messages"),
        "claude-sonnet-5-5",
        "sys",
    )
    .with_thinking(ThinkingSupport {
        adaptive: true,
        off: ThinkingOff::BetweenTools,
    })
    .without_thinking()
    .with_stream_ceiling(std::time::Duration::from_secs(30));
    client.set_abort(std::sync::Arc::clone(&interrupt));
    let presser = std::sync::Arc::clone(&interrupt);
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        presser.notify_waiters();
    });

    let turns = [WireTurn::text("is this safe?")];
    let completion = client
        .complete(&turns, &[], ToolChoice::auto(), |_| {})
        .await
        .expect("an interrupt is not an open failure");
    assert!(
        completion.operator_abort(),
        "the retry ended on the interrupt"
    );
    assert!(
        completion
            .upstream_failure()
            .is_some_and(|why| why.contains("interrupt")),
        "{:?}",
        completion.upstream_failure()
    );
    assert_eq!(
        upstream.requests().len(),
        2,
        "the interrupt ended the recovery: no third request"
    );

    client
        .complete(&turns, &[], ToolChoice::auto(), |_| {})
        .await
        .unwrap();
    let bodies = request_bodies(&upstream);
    assert_eq!(bodies.len(), 4);
    assert_eq!(bodies[2]["thinking"]["type"], "between_tools", "unlearned");
    assert_eq!(bodies[3]["thinking"]["type"], "adaptive");
    assert_eq!(bodies[3]["output_config"]["effort"], "low");
}

/// After the recovery, a later forced call and its keepalive ping go out in the same
/// recovered form: `auto` with the instruction, adaptive at `low`. The ping reads the
/// record as its body is built, so both bodies take their thinking and effort from it.
#[tokio::test]
async fn the_keepalive_pings_the_recovered_form() {
    let pong = r#"{"id":"p","model":"m","usage":{"input_tokens":1,"output_tokens":0}}"#;
    let (client, server) = scripted(vec![
        (400, REFUSAL),
        (200, STREAM),
        (200, STREAM),
        (200, pong),
    ]);
    let client = client.without_thinking();
    let turns = vec![WireTurn::text("is this safe?")];
    let forced = ToolChoice::force("spot");
    client
        .complete(&turns, &[], ToolChoice::auto(), |_| {})
        .await
        .unwrap();
    client.complete(&turns, &[], forced, |_| {}).await.unwrap();
    let sent = client.sent_form(turns, &[], forced);
    client
        .keepalive(RequestOrigin::Keepalive, &sent)
        .await
        .unwrap();

    let bodies = server.bodies();
    let (later, ping) = (&bodies[2], &bodies[3]);
    assert_eq!(later["tool_choice"]["type"], "auto", "{later}");
    assert_eq!(later["thinking"]["type"], "adaptive", "{later}");
    assert_eq!(later["output_config"]["effort"], "low", "{later}");
    assert!(
        last_user_text(later).contains("calling the `spot` tool"),
        "{later}"
    );
    assert_eq!(
        ping["thinking"], later["thinking"],
        "the ping sends the turn's envelope"
    );
    assert_eq!(ping["output_config"], later["output_config"]);
    assert_eq!(ping["tool_choice"], later["tool_choice"]);
}
