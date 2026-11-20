//! The prose rung's requests (docs/permissions.md): the cap and effort they carry, an
//! answer without a verdict asked again once, and a refused effort dropped.

use super::*;
use crate::inference::api::{StageRecorder, StageSink, ThinkingOff, ThinkingSupport};
use crate::inference::testing::upstream::{Canned, Request, ScriptedUpstream, end_turn, events};
use crate::record::log::{Provider, RequestOrigin, RequestStage, RequestStageKind, Stance};
use std::sync::atomic::AtomicU64;
use std::sync::{Arc, Mutex};

/// A Messages answer calling `spot` with `stance`.
fn spot_call(stance: &str) -> String {
    crate::inference::testing::upstream::tool_call(
        "c1",
        SPOT,
        &json!({ "stance": stance, "reason": "fine" }).to_string(),
    )
}

/// A Messages answer the cap cut while the model was still writing prose.
fn cut_by_the_cap() -> String {
    events(&[
        r#"{"type":"message_start","message":{"model":"m","usage":{"input_tokens":1,"output_tokens":0}}}"#.to_string(),
        r#"{"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}"#.to_string(),
        r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"thinking it over"}}"#.to_string(),
        r#"{"type":"content_block_stop","index":0}"#.to_string(),
        r#"{"type":"message_delta","delta":{"stop_reason":"max_tokens"},"usage":{"output_tokens":7}}"#.to_string(),
        r#"{"type":"message_stop"}"#.to_string(),
    ])
}

/// A Responses answer calling `spot`.
fn responses_spot_call() -> String {
    let arguments = serde_json::to_string(r#"{"stance":"clear","reason":"fine"}"#).unwrap();
    format!(
        "event: response.completed\ndata: {{\"type\":\"response.completed\",\"response\":\
         {{\"id\":\"r\",\"status\":\"completed\",\"output\":[{{\"type\":\"function_call\",\
         \"call_id\":\"c1\",\"name\":\"spot\",\"arguments\":{arguments}}}],\
         \"usage\":{{\"input_tokens\":1,\"output_tokens\":1}}}}}}\n\n"
    )
}

/// A spotter on `provider`, answered by `upstream` in order.
fn spotter(upstream: &ScriptedUpstream, provider: Provider, path: &str) -> ForkSpot {
    ForkSpot::new(upstream.endpoint(provider, path), "m")
}

/// A Messages spotter whose upstream answers `replies` in order, as event streams.
fn messages_spotter(replies: Vec<String>) -> (ForkSpot, ScriptedUpstream) {
    let upstream = ScriptedUpstream::answering(replies.into_iter().map(Canned::stream));
    let spot = spotter(&upstream, Provider::Anthropic, "/v1/messages").with_capabilities(
        &ModelCapabilities {
            thinking: ThinkingSupport {
                adaptive: true,
                off: ThinkingOff::Disabled,
            },
            ..ModelCapabilities::unknown()
        },
    );
    (spot, upstream)
}

async fn assess(spot: &ForkSpot) -> Spotting {
    let call = ToolCall::new(BASH, "x", json!({ "command": "ls" }));
    spot.assess(&Config::default(), &call, &Circumstances::default())
        .await
}

fn bodies(upstream: &ScriptedUpstream) -> Vec<Value> {
    upstream.requests().iter().map(Request::json).collect()
}

/// **An answer without a `spot` call is asked again once**, and the verdict that
/// follows is the model's, with both requests' tokens in its usage.
#[tokio::test]
async fn an_answer_without_a_verdict_is_asked_again() {
    let (spot, upstream) = messages_spotter(vec![end_turn(), spot_call("clear")]);
    let spotting = assess(&spot).await;
    assert_eq!(spotting.origin, Origin::Model);
    assert_eq!(spotting.read.stance, Stance::Clear);
    assert_eq!(upstream.count(), 2);
    let usage = spotting.usage.expect("both requests were answered");
    assert_eq!(usage.usage.input_tokens, 2);
    assert_eq!(usage.usage.output_tokens, 2);
}

/// **Two answers without a verdict fail closed**, after the second: the retry is
/// spent once, so a model that never calls `spot` reaches the human after two requests
/// rather than the whole budget.
#[tokio::test]
async fn two_answers_without_a_verdict_fail_closed() {
    let (spot, upstream) = messages_spotter(vec![end_turn(), end_turn(), spot_call("clear")]);
    let spotting = assess(&spot).await;
    assert_eq!(spotting.origin, Origin::Fallback);
    assert_eq!(spotting.read.stance, Stance::Gate);
    assert_eq!(upstream.count(), 2);
    assert_eq!(spotting.usage.expect("answered").usage.output_tokens, 2);
}

/// **An answer the cap cut is asked again at a raised cap**: the spotter's own cap,
/// then twice it.
#[tokio::test]
async fn an_answer_the_cap_cut_is_asked_again_at_a_raised_cap() {
    let (spot, upstream) = messages_spotter(vec![cut_by_the_cap(), spot_call("watch")]);
    let spotting = assess(&spot).await;
    assert_eq!(spotting.origin, Origin::Model);
    assert_eq!(spotting.read.stance, Stance::Watch);
    let caps: Vec<Value> = bodies(&upstream)
        .iter()
        .map(|b| b["max_tokens"].clone())
        .collect();
    assert_eq!(caps, [json!(SPOT_MAX_OUTPUT), json!(SPOT_MAX_OUTPUT * 2)]);
}

/// **A dropped stream is asked again after a pause**, and its verdict is the model's.
#[tokio::test(start_paused = true)]
async fn a_dropped_stream_is_asked_again_after_a_pause() {
    let upstream =
        ScriptedUpstream::answering([Canned::stream(""), Canned::stream(spot_call("clear"))]);
    let spot = spotter(&upstream, Provider::Anthropic, "/v1/messages");
    let started = tokio::time::Instant::now();
    let spotting = assess(&spot).await;
    assert_eq!(spotting.origin, Origin::Model);
    assert_eq!(upstream.count(), 2);
    assert!(started.elapsed() >= Duration::from_secs(1));
}

/// **The model's cap bounds the spotter's**: below [`SPOT_MAX_OUTPUT`] it is sent as
/// is, and a retry cannot raise it past it.
#[test]
fn the_models_cap_bounds_the_spotters() {
    let spot = ForkSpot::new(Endpoint::without_url(Provider::Anthropic), "m").with_capabilities(
        &ModelCapabilities {
            max_output_tokens: Some(8_000),
            ..ModelCapabilities::unknown()
        },
    );
    let call = ToolCall::new(BASH, "x", json!({}));
    let body: Value = serde_json::from_str(
        &spot
            .request_body(&Config::default(), &call, &Circumstances::default())
            .unwrap(),
    )
    .unwrap();
    assert_eq!(body["max_tokens"], 8_000);
    assert_eq!(spot.retry_cap, 8_000);
}

/// **On Responses the request asks for a low effort**, with no summary, whatever the
/// model's metadata declares about reasoning; on Messages it asks for none and sends
/// thinking off.
#[test]
fn only_a_responses_spotter_asks_for_an_effort() {
    let call = ToolCall::new(BASH, "x", json!({}));
    let body = |provider| -> Value {
        let spot = ForkSpot::new(Endpoint::without_url(provider), "m")
            .with_capabilities(&ModelCapabilities::unknown());
        serde_json::from_str(
            &spot
                .request_body(&Config::default(), &call, &Circumstances::default())
                .unwrap(),
        )
        .unwrap()
    };
    let responses = body(Provider::Custom(WireDialect::Responses));
    assert_eq!(responses["reasoning"], json!({ "effort": SPOT_EFFORT }));
    assert_eq!(responses["max_output_tokens"], SPOT_MAX_OUTPUT);
    let messages = body(Provider::Anthropic);
    assert!(messages.get("output_config").is_none(), "{messages}");
    assert!(messages.get("reasoning").is_none(), "{messages}");
}

/// **A refused effort is dropped and remembered.** The endpoint rejects every request
/// carrying it, forced or not; the request without it is answered, and the next
/// assessment sends none from the start.
#[tokio::test]
async fn a_refused_effort_is_dropped_and_remembered() {
    let refusal = r#"{"error":{"message":"reasoning.effort is not supported with this model"}}"#;
    let upstream = ScriptedUpstream::answering([
        Canned::json(400, refusal),
        Canned::json(400, refusal),
        Canned::stream(responses_spot_call()),
        Canned::stream(responses_spot_call()),
    ]);
    let spot = spotter(
        &upstream,
        Provider::Custom(WireDialect::Responses),
        "/v1/responses",
    );
    assert_eq!(assess(&spot).await.origin, Origin::Model);
    assert_eq!(assess(&spot).await.origin, Origin::Model);
    let sent: Vec<(Value, bool)> = bodies(&upstream)
        .iter()
        .map(|b| (b["tool_choice"].clone(), b.get("reasoning").is_some()))
        .collect();
    let forced = json!({ "type": "function", "name": SPOT });
    assert_eq!(
        sent,
        [
            // The forced request, then the client's forced-choice fallback, both with
            // the effort and both refused.
            (forced.clone(), true),
            (json!("auto"), true),
            // Without the effort, forced again: answered.
            (forced.clone(), false),
            // The next assessment.
            (forced, false),
        ]
    );
}

#[derive(Default)]
struct Recorder {
    next: AtomicU64,
    stages: Mutex<Vec<RequestStage>>,
}

impl StageRecorder for Recorder {
    fn mint(&self) -> u64 {
        self.next.fetch_add(1, Ordering::SeqCst)
    }
    fn record(&self, stage: RequestStage) {
        self.stages.lock().unwrap().push(stage);
    }
}

/// **The retry is recorded like the first request**: its own request id under the
/// spotter's origin, and the first request's end names why it did not answer.
#[tokio::test]
async fn the_retry_is_recorded_as_its_own_spot_request() {
    let (mut spot, _upstream) = messages_spotter(vec![cut_by_the_cap(), spot_call("clear")]);
    let recorder = Arc::new(Recorder::default());
    spot.set_stage_sink(Arc::clone(&recorder) as StageSink);
    assert_eq!(assess(&spot).await.origin, Origin::Model);
    let stages: Vec<(u64, RequestOrigin, RequestStageKind, Option<String>)> = recorder
        .stages
        .lock()
        .unwrap()
        .iter()
        .map(|s| (s.request, s.origin, s.stage.clone(), s.finish.clone()))
        .collect();
    let spot_stage = |request, stage, finish: Option<&str>| {
        (
            request,
            RequestOrigin::Spot,
            stage,
            finish.map(str::to_string),
        )
    };
    assert_eq!(
        stages,
        [
            spot_stage(0, RequestStageKind::Sent, None),
            spot_stage(0, RequestStageKind::FirstByte, None),
            spot_stage(0, RequestStageKind::Ended, Some("max_tokens")),
            spot_stage(1, RequestStageKind::Sent, None),
            spot_stage(1, RequestStageKind::FirstByte, None),
            spot_stage(1, RequestStageKind::Ended, Some("tool_use")),
        ]
    );
}
