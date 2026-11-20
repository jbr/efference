//! The first-party route's size refusal, re-sent once with the long-context header
//! (docs/transport.md), against a scripted upstream.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use super::*;
use crate::inference::api::StageRecorder;
use crate::inference::testing::upstream::{Canned, ScriptedUpstream};
use crate::record::log::{RequestStage, RequestStageKind};

const HEADER: &str = "TypeSafe-Danger-Zone-Long-Context";

#[derive(Default)]
struct Recorder {
    next: AtomicU64,
    stages: Mutex<Vec<RequestStageKind>>,
}

impl StageRecorder for Recorder {
    fn mint(&self) -> u64 {
        self.next.fetch_add(1, Ordering::SeqCst)
    }
    fn record(&self, stage: RequestStage) {
        self.stages.lock().unwrap().push(stage.stage);
    }
}

/// The route's refusal of a state over its default size limits, as it answered one.
fn too_long() -> Canned {
    Canned::json(400, r#"{"detail":{"error_type":"max_tokens_exceeded"}}"#)
}

fn answered() -> Canned {
    Canned::json(
        200,
        r#"{"model":"jev-1.14.0","answers":{"asked":{"type":"noul","noul":0.9}},"usage":{"input_tokens":70000}}"#,
    )
}

/// Ask `provider`'s route, answered by `upstream`, recording the stages.
async fn decide(
    provider: Provider,
    upstream: &ScriptedUpstream,
) -> (Result<Decisions, Error>, Vec<RequestStageKind>) {
    let recorder = Arc::new(Recorder::default());
    let sink: StageSink = Arc::clone(&recorder) as StageSink;
    let questions = BTreeMap::from([(
        "asked".to_string(),
        Question::boolean("Was it asked for?", "Yes.", "No."),
    )]);
    let result = upstream
        .endpoint(provider, "/unused")
        .decide_traced(
            Some(&sink),
            RequestOrigin::Spot,
            "jev-1.14.0",
            &serde_json::json!({ "mission": "work here" }),
            &questions,
        )
        .await;
    let stages = recorder.stages.lock().unwrap().clone();
    (result, stages)
}

/// A first-party refusal for size is sent again once, at once, the same body with the
/// header that raises the limits; the first attempt carries no header, and the retry
/// is recorded as a failed attempt of the one request.
#[tokio::test]
async fn a_first_party_size_refusal_is_sent_again_with_the_long_context_header() {
    let upstream = ScriptedUpstream::answering([too_long(), answered()]);
    let (result, stages) = decide(Provider::TypeSafe, &upstream).await;
    let decisions = result.expect("the re-send is answered");
    assert_eq!(decisions.probability("asked"), Some(0.9));

    let requests = upstream.requests();
    assert_eq!(requests.len(), 2, "{}", upstream.describe());
    assert_eq!(requests[0].header(HEADER), None);
    assert_eq!(requests[1].header(HEADER), Some("true"));
    assert_eq!(requests[0].body, requests[1].body);
    assert_eq!(
        stages,
        [
            RequestStageKind::Sent,
            RequestStageKind::AttemptFailed {
                attempt: 1,
                status: 400
            },
            RequestStageKind::FirstByte,
            RequestStageKind::Ended,
        ]
    );
}

/// The header raises the limits once: a re-send refused for size again fails, and the
/// gate falls to the human.
#[tokio::test]
async fn a_second_size_refusal_fails() {
    let upstream = ScriptedUpstream::answering([too_long(), too_long()]);
    let (result, stages) = decide(Provider::TypeSafe, &upstream).await;
    let error = result.expect_err("refused twice");
    assert!(error.to_string().contains("max_tokens_exceeded"), "{error}");
    assert_eq!(upstream.count(), 2);
    assert!(matches!(
        stages.as_slice(),
        [
            RequestStageKind::Sent,
            RequestStageKind::AttemptFailed { .. },
            RequestStageKind::FirstByte,
            RequestStageKind::Failed { .. },
        ]
    ));
}

/// Another 400 is not a size refusal, and is not sent again.
#[tokio::test]
async fn another_bad_request_is_not_sent_again() {
    let upstream = ScriptedUpstream::answering([Canned::json(
        400,
        r#"{"detail":{"error_type":"invalid_question"}}"#,
    )]);
    let (result, _) = decide(Provider::TypeSafe, &upstream).await;
    let error = result.expect_err("refused");
    assert!(error.to_string().contains("invalid_question"), "{error}");
    assert_eq!(upstream.count(), 1);
}

/// Only the first-party route is sent the header: OpenRouter's size refusal fails as
/// any refusal does, and its one request carries no header.
#[tokio::test]
async fn other_providers_are_never_sent_the_header() {
    for provider in [
        Provider::OpenRouter,
        Provider::Custom(WireDialect::SystemOne),
    ] {
        let upstream = ScriptedUpstream::answering([too_long()]);
        let mut endpoint = upstream.endpoint(provider, "/v1/systemone");
        // A described System One endpoint's route is its configured URL.
        endpoint.base = Some(ScriptedUpstream::url("/v1/systemone"));
        let result = endpoint
            .decide("m", &serde_json::json!({}), &BTreeMap::new())
            .await;
        assert!(result.is_err(), "{provider}");
        let requests = upstream.requests();
        assert_eq!(requests.len(), 1, "{provider}: {}", upstream.describe());
        assert_eq!(requests[0].header(HEADER), None, "{provider}");
    }
}
