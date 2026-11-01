//! Tests for the request-stage interface (`api/stages.rs`, docs/log-and-projection.md):
//! the stages a request passes, in order, for a normal request, a retried one, an
//! interrupted one, one the interrupt stopped before it was sent, a cancelled one, and two
//! that overlap.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use super::*;
use crate::inference::testing::upstream::{Canned, ScriptedUpstream};
use crate::record::log::{RequestOrigin, RequestStage, RequestStageKind};

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

impl Recorder {
    /// The kinds recorded for one request, in order.
    fn kinds(&self, request: u64) -> Vec<RequestStageKind> {
        self.stages
            .lock()
            .unwrap()
            .iter()
            .filter(|s| s.request == request)
            .map(|s| s.stage.clone())
            .collect()
    }
}

const FINALIZED: &str = "event: message_start\ndata: {\"type\":\"message_start\",\
    \"message\":{\"id\":\"gen-1\",\"model\":\"m\",\
    \"usage\":{\"input_tokens\":1,\"output_tokens\":0}}}\n\n\
    event: message_delta\ndata: {\"delta\":{\"stop_reason\":\"end_turn\"},\
    \"usage\":{\"input_tokens\":1,\"output_tokens\":1}}\n\n\
    event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n";

fn finalized() -> Canned {
    Canned::stream(FINALIZED)
}

fn unavailable() -> Canned {
    Canned::json(503, "{\"error\":{\"message\":\"overloaded\"}}")
}

fn client(upstream: &ScriptedUpstream, recorder: &Arc<Recorder>) -> Client {
    let mut client = Client::new(
        upstream.endpoint(Provider::Anthropic, "/v1/messages"),
        "m",
        "sys",
    );
    client.set_stage_sink(Arc::clone(recorder) as StageSink);
    client
}

/// A request that completes records sent, first byte, ended.
#[tokio::test]
async fn a_normal_request_records_sent_first_byte_ended() {
    let upstream = ScriptedUpstream::answering([finalized()]);
    let recorder = Arc::new(Recorder::default());
    client(&upstream, &recorder)
        .with_origin(RequestOrigin::Turn)
        .complete(&[], &[], ToolChoice::auto(), |_| {})
        .await
        .unwrap();

    assert_eq!(
        recorder.kinds(0),
        [
            RequestStageKind::Sent,
            RequestStageKind::FirstByte,
            RequestStageKind::Ended
        ]
    );
    assert!(
        recorder
            .stages
            .lock()
            .unwrap()
            .iter()
            .all(|s| s.origin == RequestOrigin::Turn)
    );
    // The ended stage keeps the provider's own word for the finish; no other
    // stage carries one.
    let finishes: Vec<_> = recorder
        .stages
        .lock()
        .unwrap()
        .iter()
        .map(|s| s.finish.clone())
        .collect();
    assert_eq!(finishes, [None, None, Some("end_turn".to_string())]);
}

/// A refused attempt the transport retries is recorded between sent and first
/// byte, and the request still ends once.
#[tokio::test(start_paused = true)]
async fn a_retried_request_records_the_refused_attempt() {
    let upstream = ScriptedUpstream::answering([unavailable(), finalized()]);
    let recorder = Arc::new(Recorder::default());
    client(&upstream, &recorder)
        .with_origin(RequestOrigin::Triage)
        .complete(&[], &[], ToolChoice::auto(), |_| {})
        .await
        .unwrap();
    assert_eq!(upstream.count(), 2);

    assert_eq!(
        recorder.kinds(0),
        [
            RequestStageKind::Sent,
            RequestStageKind::AttemptFailed {
                attempt: 1,
                status: 503
            },
            RequestStageKind::FirstByte,
            RequestStageKind::Ended
        ]
    );
}

/// The operator's interrupt closes the request as `failed: interrupted`.
#[tokio::test(start_paused = true)]
async fn an_interrupted_request_records_failed_interrupted() {
    let event = "event: message_start\ndata: {\"message\":{\"model\":\"m\",\
                 \"usage\":{\"input_tokens\":1,\"output_tokens\":0}}}\n\n";
    let upstream = ScriptedUpstream::answering([Canned::stream(event).left_open()]);
    let recorder = Arc::new(Recorder::default());
    let interrupt = Arc::new(tokio::sync::Notify::new());
    let mut client = client(&upstream, &recorder);
    client.set_abort(Arc::clone(&interrupt));
    let presser = Arc::clone(&interrupt);
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(150)).await;
        presser.notify_waiters();
    });
    client
        .with_origin(RequestOrigin::Turn)
        .complete(&[], &[], ToolChoice::auto(), |_| {})
        .await
        .unwrap();

    assert_eq!(
        recorder.kinds(0),
        [
            RequestStageKind::Sent,
            RequestStageKind::FirstByte,
            RequestStageKind::Failed {
                reason: "interrupted".into()
            }
        ]
    );
}

/// A press as `Session::interrupt` makes one: the latch set, then the notifier fired.
fn press(interrupt: &tokio::sync::Notify, latch: &AtomicBool) {
    latch.store(true, Ordering::SeqCst);
    interrupt.notify_waiters();
}

/// A client whose reads answer to `interrupt`, as the runtime wires the agent's.
fn interruptible(
    upstream: &ScriptedUpstream,
    recorder: &Arc<Recorder>,
    interrupt: &Arc<tokio::sync::Notify>,
) -> Client {
    let mut client = client(upstream, recorder);
    client.set_abort(Arc::clone(interrupt));
    client
}

/// The loop's model request reads the latch once its abort is armed. A press that landed
/// before then — the notifier fired with nothing waiting, the latch left set — stops it
/// unsent: nothing reaches the upstream and no stage is recorded, and it comes back as a
/// read the interrupt ended.
#[tokio::test(start_paused = true)]
async fn a_request_carrying_a_pressed_latch_is_not_sent() {
    let upstream = ScriptedUpstream::answering([finalized()]);
    let recorder = Arc::new(Recorder::default());
    let interrupt = Arc::new(tokio::sync::Notify::new());
    let latch = Arc::new(AtomicBool::new(false));
    press(&interrupt, &latch);
    let completion = interruptible(&upstream, &recorder, &interrupt)
        .with_origin(RequestOrigin::Turn)
        .with_interrupt_latch(Arc::clone(&latch))
        .complete(&[], &[], ToolChoice::auto(), |_| {})
        .await
        .unwrap();

    assert!(completion.operator_abort());
    assert_eq!(
        completion.upstream_failure(),
        Some("generation interrupted")
    );
    assert_eq!(upstream.count(), 0, "nothing was sent");
    assert!(recorder.stages.lock().unwrap().is_empty());
}

/// A request that does not carry the latch — a keepalive, a triage, a title — is sent past
/// a press still latched, which is not its own, and ends.
#[tokio::test(start_paused = true)]
async fn a_request_without_the_latch_is_sent_past_a_latched_press() {
    let upstream = ScriptedUpstream::answering([finalized()]);
    let recorder = Arc::new(Recorder::default());
    let interrupt = Arc::new(tokio::sync::Notify::new());
    let latch = AtomicBool::new(false);
    press(&interrupt, &latch);
    let completion = interruptible(&upstream, &recorder, &interrupt)
        .with_origin(RequestOrigin::Triage)
        .complete(&[], &[], ToolChoice::auto(), |_| {})
        .await
        .unwrap();

    assert!(!completion.operator_abort());
    assert_eq!(upstream.count(), 1);
    assert_eq!(
        recorder.kinds(0),
        [
            RequestStageKind::Sent,
            RequestStageKind::FirstByte,
            RequestStageKind::Ended
        ]
    );
}

/// A press during the stream still ends a request carrying the latch: the latch read
/// before sending found it clear, and the armed abort ends the read.
#[tokio::test(start_paused = true)]
async fn a_press_mid_stream_ends_a_request_carrying_the_latch() {
    let event = "event: message_start\ndata: {\"message\":{\"model\":\"m\",\
                 \"usage\":{\"input_tokens\":1,\"output_tokens\":0}}}\n\n";
    let upstream = ScriptedUpstream::answering([Canned::stream(event).left_open()]);
    let recorder = Arc::new(Recorder::default());
    let interrupt = Arc::new(tokio::sync::Notify::new());
    let latch = Arc::new(AtomicBool::new(false));
    let (presser, pressed) = (Arc::clone(&interrupt), Arc::clone(&latch));
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(150)).await;
        press(&presser, &pressed);
    });
    let completion = interruptible(&upstream, &recorder, &interrupt)
        .with_origin(RequestOrigin::Turn)
        .with_interrupt_latch(latch)
        .complete(&[], &[], ToolChoice::auto(), |_| {})
        .await
        .unwrap();

    assert!(completion.operator_abort());
    assert_eq!(
        recorder.kinds(0),
        [
            RequestStageKind::Sent,
            RequestStageKind::FirstByte,
            RequestStageKind::Failed {
                reason: "interrupted".into()
            }
        ]
    );
}

/// A request whose future is dropped before it finishes does not stay open.
#[tokio::test(start_paused = true)]
async fn a_cancelled_request_closes_as_interrupted() {
    // The upstream takes the request and says nothing until the client gives up.
    let upstream = ScriptedUpstream::answering([Canned::silent()]);
    let recorder = Arc::new(Recorder::default());
    let client = client(&upstream, &recorder).with_origin(RequestOrigin::Title);
    let cancelled = tokio::time::timeout(
        Duration::from_millis(200),
        client.complete(&[], &[], ToolChoice::auto(), |_| {}),
    )
    .await;
    assert!(cancelled.is_err(), "the upstream never answers");

    assert_eq!(
        recorder.kinds(0),
        [
            RequestStageKind::Sent,
            RequestStageKind::Failed {
                reason: "interrupted".into()
            }
        ]
    );
}

/// Two requests in flight at once keep their own ids and origins; their stages
/// interleave in the order they happened.
#[tokio::test]
async fn concurrent_requests_interleave_by_id() {
    let upstream = ScriptedUpstream::answering([finalized(), finalized()]);
    let recorder = Arc::new(Recorder::default());
    let base = client(&upstream, &recorder);
    let triage = base.with_origin(RequestOrigin::Triage);
    let title = base.with_origin(RequestOrigin::Title);
    let (a, b) = tokio::join!(
        triage.complete(&[], &[], ToolChoice::auto(), |_| {}),
        title.complete(&[], &[], ToolChoice::auto(), |_| {}),
    );
    a.unwrap();
    b.unwrap();

    let stages = recorder.stages.lock().unwrap().clone();
    assert_eq!(stages.len(), 6);
    for request in [0, 1] {
        let own: Vec<_> = stages.iter().filter(|s| s.request == request).collect();
        assert_eq!(own.len(), 3, "each request records its own three stages");
        assert!(own.iter().all(|s| s.origin == own[0].origin));
        assert_eq!(own[2].stage, RequestStageKind::Ended);
    }
    let origins: std::collections::HashSet<_> = stages.iter().map(|s| s.origin).collect();
    assert_eq!(origins.len(), 2);
}

/// A triage primer whose capped output cuts the forced call mid-arguments is its
/// expected end: the stage is `ended` with the finish word, not `failed`. One that
/// accounted no input planted nothing and stays `failed` (docs/transport.md).
#[tokio::test]
async fn a_truncated_primer_ends_and_an_empty_one_fails() {
    for (input_tokens, planted) in [(1000, true), (0, false)] {
        let body = format!(
            "event: response.completed\ndata: {{\"type\":\"response.completed\",\
             \"response\":{{\"id\":\"r\",\"status\":\"incomplete\",\
             \"incomplete_details\":{{\"reason\":\"max_output_tokens\"}},\
             \"output\":[{{\"type\":\"function_call\",\"call_id\":\"c\",\
             \"name\":\"retain\",\"arguments\":\"{{not json\"}}],\
             \"usage\":{{\"input_tokens\":{input_tokens},\"output_tokens\":16}}}}}}\n\n"
        );
        let upstream = ScriptedUpstream::answering([Canned::stream(body)]);
        let recorder = Arc::new(Recorder::default());
        let mut endpoint = upstream.endpoint(Provider::Anthropic, "/v1/messages");
        endpoint.provider = Provider::OpenAICodex;
        let mut client = Client::new(endpoint, "gpt-6-luna", "sys")
            .with_prompt_cache(PromptCachePolicy::Implicit);
        client.set_stage_sink(Arc::clone(&recorder) as StageSink);
        let turns = vec![crate::record::project::WireTurn::text("hello")];
        let primed = client
            .prime(&turns, &[], ToolChoice::auto())
            .await
            .expect("an implicit Responses lineage takes a primer");
        assert_eq!(upstream.count(), 1);

        assert_eq!(primed.is_ok(), planted);
        let stages = recorder.stages.lock().unwrap().clone();
        let last = stages.last().unwrap();
        assert_eq!(last.origin, RequestOrigin::TriagePrimer);
        if planted {
            assert_eq!(last.stage, RequestStageKind::Ended);
            assert_eq!(last.finish.as_deref(), Some("incomplete:max_output_tokens"));
        } else {
            assert!(matches!(last.stage, RequestStageKind::Failed { .. }));
        }
    }
}
