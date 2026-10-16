//! The decision-rung spotter's request stages (docs/log-and-projection.md): its decisions
//! request records `sent`, `first_byte`, and `ended` under the `spot` origin, or `failed`
//! when the route refuses it, against a scripted endpoint.

use super::*;
use crate::api::{Provider, StageRecorder, StageSink, WireDialect};
use crate::log::{RequestStage, RequestStageKind};
use std::io::{Read, Write};
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

/// An endpoint answering its one request with `status` and `body`.
fn scripted(status: &str, body: String) -> Endpoint {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}/v1/decisions", listener.local_addr().unwrap());
    let status = status.to_string();
    std::thread::spawn(move || {
        let (mut sock, _) = listener.accept().unwrap();
        let mut buf = [0u8; 65536];
        let _ = sock.read(&mut buf);
        let _ = write!(
            sock,
            "HTTP/1.1 {status}\r\ncontent-type: application/json\r\nconnection: close\r\n\
             content-length: {}\r\n\r\n{body}",
            body.len()
        );
        let _ = sock.flush();
    });
    Endpoint::without_credential(Provider::Custom(WireDialect::SystemOne), base)
}

/// Every unconditional axis answered, sketch at level 0.
fn clean_answers() -> String {
    json!({
        "model": "jev",
        "answers": {
            "in_scope": { "type": "noul", "noul": 0.95 },
            "path_outside": { "type": "noul", "noul": 0.02 },
            "path_denied": { "type": "noul", "noul": 0.01 },
            "exposes_secrets": { "type": "noul", "noul": 0.01 },
            "sketch": { "type": "score", "probabilities":
                { "0": 0.8, "1": 0.05, "2": 0.05, "3": 0.05, "4": 0.05 } },
        },
        "usage": { "input_tokens": 10, "output_tokens": 0 },
    })
    .to_string()
}

async fn assess(endpoint: Endpoint, recorder: &Arc<Recorder>) -> Spotting {
    let mut spot = JevSpot::new(endpoint, "jev");
    spot.set_stage_sink(Arc::clone(recorder) as StageSink);
    let call = ToolCall::new(crate::bash::NAME, "c1", json!({ "command": "ls" }));
    spot.assess(&role(""), &call, None, &[], None, &[]).await
}

fn recorded(recorder: &Recorder) -> Vec<(u64, RequestOrigin, RequestStageKind)> {
    let stages = recorder.stages.lock().unwrap();
    stages
        .iter()
        .map(|s| (s.request, s.origin, s.stage.clone()))
        .collect()
}

/// **A decision records the stages every request does.** One decisions request, one
/// request id, under the spotter's origin.
#[tokio::test]
async fn a_decision_records_sent_first_byte_ended() {
    let recorder = Arc::new(Recorder::default());
    let spotting = assess(scripted("200 OK", clean_answers()), &recorder).await;
    assert_eq!(spotting.origin, Origin::Model);
    assert_eq!(
        recorded(&recorder),
        vec![
            (0, RequestOrigin::Spot, RequestStageKind::Sent),
            (0, RequestOrigin::Spot, RequestStageKind::FirstByte),
            (0, RequestOrigin::Spot, RequestStageKind::Ended),
        ]
    );
}

/// **A refused request records `failed`** with the error, and the rung still fails
/// closed.
#[tokio::test]
async fn a_refused_decision_records_failed() {
    let recorder = Arc::new(Recorder::default());
    let spotting = assess(
        scripted("400 Bad Request", r#"{"error":"nope"}"#.into()),
        &recorder,
    )
    .await;
    assert_eq!(spotting.origin, Origin::Fallback);
    let stages = recorded(&recorder);
    assert_eq!(stages.len(), 3, "{stages:?}");
    assert_eq!(stages[0].2, RequestStageKind::Sent);
    assert_eq!(stages[1].2, RequestStageKind::FirstByte);
    match &stages[2].2 {
        RequestStageKind::Failed { reason } => {
            assert!(reason.contains("400"), "{reason}");
        }
        other => panic!("expected failed, got {other:?}"),
    }
}
