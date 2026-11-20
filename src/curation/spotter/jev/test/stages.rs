//! The decision-rung spotter's request stages (docs/log-and-projection.md): each of its
//! two decisions requests records `sent`, `first_byte`, and `ended` under the `spot`
//! origin, or `failed` when the route refuses it, against a scripted endpoint.

use super::requests::{answered, endpoint, response};
use super::*;
use crate::inference::api::{StageRecorder, StageSink};
use crate::inference::testing::upstream::{Canned, ScriptedUpstream};
use crate::record::log::{RequestStage, RequestStageKind};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

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

async fn assess(endpoint: Endpoint, recorder: &Arc<Recorder>) -> Spotting {
    let mut spot = JevSpot::new(endpoint, "jev");
    spot.set_stage_sink(Arc::clone(recorder) as StageSink);
    let call = ToolCall::new(
        crate::record::names::tools::BASH,
        "c1",
        json!({ "command": "ls" }),
    );
    spot.assess(&Config::default(), &call, &Circumstances::default())
        .await
}

/// Each request's stages in the order it recorded them, by request id. The two
/// requests run concurrently, so their stages interleave in no fixed order.
fn by_request(recorder: &Recorder) -> Vec<Vec<RequestStageKind>> {
    let stages = recorder.stages.lock().unwrap();
    let mut requests: Vec<Vec<RequestStageKind>> = Vec::new();
    for stage in stages.iter() {
        assert_eq!(stage.origin, RequestOrigin::Spot);
        let id = usize::try_from(stage.request).expect("a small id");
        if requests.len() <= id {
            requests.resize(id + 1, Vec::new());
        }
        requests[id].push(stage.stage.clone());
    }
    requests
}

/// **A decision records the stages every request does.** Two decisions requests, two
/// request ids, each under the spotter's origin.
#[tokio::test]
async fn each_decisions_request_records_sent_first_byte_ended() {
    let recorder = Arc::new(Recorder::default());
    let upstream = ScriptedUpstream::new(|_| response(&answered(0.5)));
    let spotting = assess(endpoint(&upstream), &recorder).await;
    assert_eq!(spotting.origin, Origin::Model);
    let complete = vec![
        RequestStageKind::Sent,
        RequestStageKind::FirstByte,
        RequestStageKind::Ended,
    ];
    assert_eq!(by_request(&recorder), vec![complete.clone(), complete]);
}

/// **A refused request records `failed`** with the error, and the rung still fails
/// closed.
#[tokio::test]
async fn a_refused_decision_records_failed() {
    let recorder = Arc::new(Recorder::default());
    let upstream = ScriptedUpstream::new(|_| Canned::json(400, r#"{"error":"nope"}"#));
    let spotting = assess(endpoint(&upstream), &recorder).await;
    assert_eq!(spotting.origin, Origin::Fallback);
    for stages in by_request(&recorder) {
        assert_eq!(stages.len(), 3, "{stages:?}");
        assert_eq!(stages[0], RequestStageKind::Sent);
        assert_eq!(stages[1], RequestStageKind::FirstByte);
        match &stages[2] {
            RequestStageKind::Failed { reason } => {
                assert!(reason.contains("400"), "{reason}");
            }
            other => panic!("expected failed, got {other:?}"),
        }
    }
}
