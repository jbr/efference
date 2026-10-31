//! Tests for `compact.rs` (backlog: cold-session.md).

use super::*;
use crate::daemon::compact::{COMPACTION_TAIL_TOKENS, NotCompactable, compaction_cut};
use crate::daemon::keepalive::CacheLineage;
use crate::log::{AssistantOutput, LogEntry, RawResult, Summary, Timestamp, WireState};

fn entry(seq: u64, payload: Payload) -> LogEntry {
    LogEntry {
        seq,
        turn: 1,
        at: Timestamp(1_000 + seq),
        submission_id: None,
        source: None,
        payload,
    }
}

/// A user message of about `tokens` tokens of recorded size.
fn user(seq: u64, tokens: usize) -> LogEntry {
    entry(
        seq,
        Payload::User(vec![ContentBlock::Text("x".repeat(tokens * 4))]),
    )
}

fn call(seq: u64, id: &str) -> LogEntry {
    entry(
        seq,
        Payload::Assistant {
            output: AssistantOutput::from_legacy_messages(
                vec![],
                vec![],
                vec![ToolCall::new(
                    crate::bash::NAME,
                    id,
                    serde_json::json!({"command": "ls"}),
                )],
            ),
            excluded: None,
        },
    )
}

fn result(seq: u64, id: &str, tokens: usize) -> LogEntry {
    entry(
        seq,
        Payload::tool_result(id, RawResult::text("y".repeat(tokens * 4)), WireState::Raw),
    )
}

/// The tail holds about the budget; the account covers everything before it.
#[test]
fn the_cut_leaves_a_budgeted_tail() {
    let log = [
        user(1, 5_000),
        user(2, 5_000),
        user(3, 5_000),
        user(4, 5_000),
    ];
    assert_eq!(
        compaction_cut(&log, 9_000),
        Some(2),
        "seqs 3 and 4 stay verbatim"
    );
}

/// A cut that would separate a call from its result moves earlier.
#[test]
fn the_cut_does_not_split_a_call_from_its_result() {
    let log = [user(1, 5_000), call(2, "c1"), result(3, "c1", 9_000)];
    assert_eq!(
        compaction_cut(&log, 9_000),
        Some(1),
        "the call goes with its result"
    );
}

/// Nothing to compact when everything fits, or when what is left was compacted already.
#[test]
fn nothing_to_compact_when_the_tail_holds_it_all() {
    let log = [user(1, 100), user(2, 100)];
    assert_eq!(compaction_cut(&log, COMPACTION_TAIL_TOKENS), None);

    let compacted = [
        user(1, 5_000),
        user(2, 5_000),
        entry(3, Payload::Summary(Summary::consolidation(0, 2, "account"))),
        user(4, 9_000),
    ];
    assert_eq!(
        compaction_cut(&compacted, 9_000),
        None,
        "seq 2 is already covered"
    );
}

/// After a compaction the session is in the state of one that has sent nothing: not cold,
/// and nothing for the keepalive to ping.
#[tokio::test]
async fn a_compaction_resets_the_cold_belief_and_the_keepalive() {
    let (shared, _rx) = Session::new(std::env::current_dir().unwrap(), Log::in_memory());
    shared.set_cache_ttl(Duration::ZERO);
    append_compactable_history(&shared);
    shared.record_request(RequestOrigin::Turn, RequestUsage::new(Usage::default()));
    tokio::time::sleep(Duration::from_millis(5)).await;
    assert!(shared.cold_since().is_some());
    assert!(shared.keepalive_clocks(CacheLineage::Main).0.is_some());

    shared
        .append(1, Payload::Summary(Summary::consolidation(0, 0, "account")))
        .unwrap();
    assert_eq!(shared.cold_since(), None);
    assert_eq!(shared.keepalive_clocks(CacheLineage::Main).0, None);
}

/// A warm session has nothing to compact: the compaction is the cold session's alternative to
/// an uncached read.
#[test]
fn a_compaction_needs_a_cold_session() {
    let (shared, _rx) = Session::new(std::env::current_dir().unwrap(), Log::in_memory());
    assert_eq!(
        shared.request_compaction(false),
        Err(NotCompactable::NotCold)
    );
}

/// `warm: true` is the operator's explicit request to compact a session whose cache is
/// not believed expired; the progress shows in `current_operations`, not the `cold` topic.
#[tokio::test]
async fn a_warm_compaction_is_accepted_on_request_and_reports_on_its_topic() {
    let (shared, _rx) = Session::new(std::env::current_dir().unwrap(), Log::in_memory());
    shared.set_cache_ttl(Duration::from_secs(3_600));
    append_compactable_history(&shared);
    assert_eq!(shared.cold_since(), None);
    assert_eq!(
        shared.request_compaction(false),
        Err(NotCompactable::NotCold)
    );
    assert_eq!(shared.topic_json(Topic::CurrentOperations), Value::Null);

    assert_eq!(shared.request_compaction(true), Ok(()));
    assert_eq!(
        shared.topic_json(Topic::CurrentOperations)["compaction"]["state"],
        "waiting"
    );
    assert_eq!(
        shared.topic_json(Topic::CurrentOperations)["compaction"]["warm"],
        true
    );
    assert_eq!(shared.topic_json(Topic::Cold), Value::Null);
}

/// A loop held at a triage is running but sends nothing until the operator
/// retries, so it accepts a compaction as a cold-parked loop does; one running a request
/// does not. The held triage's state was the `dry-linnet` case: a compaction refused as
/// "the loop is running" left a cold read or an archive as the only ways forward.
#[test]
fn a_held_triage_accepts_a_compaction() {
    let (shared, _rx) = Session::new(std::env::current_dir().unwrap(), Log::in_memory());
    shared.set_cache_ttl(Duration::from_secs(3_600));
    append_compactable_history(&shared);
    shared.set_phase(LoopPhase::Requesting);
    assert_eq!(shared.request_compaction(true), Err(NotCompactable::Busy));

    shared.set_stall(agent::stalled(
        StallReason::TriageUnavailable,
        None,
        "triage could not reach its model".into(),
        "429".into(),
    ));
    let _held = shared.wait_phase(
        LoopPhase::Blocked {
            reason: BlockedReason::TriageUnavailable,
        },
        LoopPhase::Requesting,
    );
    assert_eq!(shared.request_compaction(true), Ok(()));
}

/// A warm compaction with nothing past the tail still declines: warm lifts only the cache
/// guard.
#[test]
fn a_warm_compaction_still_needs_something_to_compact() {
    let (shared, _rx) = Session::new(std::env::current_dir().unwrap(), Log::in_memory());
    shared.set_cache_ttl(Duration::from_secs(3_600));
    assert_eq!(
        shared.request_compaction(true),
        Err(NotCompactable::Nothing)
    );
}

/// A failed compaction is reported until a turn's request goes out, then dropped; the
/// failure is a log entry, so it is what a reopened session reports too.
#[test]
fn a_failed_compaction_clears_when_a_turn_starts() {
    let (shared, _rx) = Session::new(std::env::current_dir().unwrap(), Log::in_memory());
    shared
        .append(
            1,
            Payload::Telemetry(crate::log::Telemetry::CompactionFailed {
                error: "boom".into(),
            }),
        )
        .unwrap();
    let failure = shared.topic_json(Topic::CompactionFailure);
    assert_eq!(failure["error"], "boom");
    assert!(failure["at"].is_u64());
    assert_eq!(
        shared.topic_json(Topic::CurrentOperations),
        Value::Null,
        "a failure is not in flight"
    );
    shared
        .append(
            1,
            Payload::Telemetry(crate::log::Telemetry::RequestStage(
                crate::log::RequestStage::new(
                    0,
                    RequestOrigin::Turn,
                    crate::log::RequestStageKind::Sent,
                ),
            )),
        )
        .unwrap();
    assert_eq!(shared.topic_json(Topic::CompactionFailure), Value::Null);
}

/// An accepted compaction is a `CompactionRequested` entry naming the cut and the warm
/// flag (docs/summary.md), so the request is in the session's record.
#[tokio::test]
async fn an_accepted_compaction_is_logged_with_its_cut() {
    let (shared, _rx) = Session::new(std::env::current_dir().unwrap(), Log::in_memory());
    shared.set_cache_ttl(Duration::from_secs(3_600));
    append_compactable_history(&shared);
    let expected = compaction_cut(&shared.snapshot(), COMPACTION_TAIL_TOKENS).unwrap();
    assert_eq!(shared.request_compaction(true), Ok(()));
    let requested: Vec<(bool, u64)> = shared
        .snapshot()
        .iter()
        .filter_map(|e| match &e.payload {
            Payload::Telemetry(crate::log::Telemetry::CompactionRequested { warm, cut }) => {
                Some((*warm, *cut))
            }
            _ => None,
        })
        .collect();
    assert_eq!(requested, [(true, expected)]);
    assert!(shared.compacting());
}

/// The client reads a refusal as a decline with the daemon's message, not an error.
#[tokio::test]
async fn the_client_reads_a_compaction_refusal_as_a_decline() {
    let (shared, _rx) = Session::new(std::env::current_dir().unwrap(), Log::in_memory());
    let client = test_client(&shared).await;
    assert_eq!(
        client.compact().await.unwrap(),
        crate::client::CompactOutcome::Declined {
            message: NotCompactable::NotCold.message().into()
        }
    );
}

/// While a compaction's account is written, the writer's thinking and text reach the
/// session's `output` broadcast as writer deltas, in the order they streamed
/// (docs/summary.md), never as the main loop's response.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_compaction_streams_its_writers_deltas() {
    use super::upstream_hold::{Answer, events, scripted, wait_until};
    let requests = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let addr = scripted(Arc::clone(&requests), |_| {
        Answer::Stream(events(&[
            r#"{"type":"message_start","message":{"model":"m","usage":{"input_tokens":1,"output_tokens":0}}}"#.to_string(),
            r#"{"type":"content_block_start","index":0,"content_block":{"type":"thinking","thinking":"","signature":""}}"#.to_string(),
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"weighing it"}}"#.to_string(),
            r#"{"type":"content_block_stop","index":0}"#.to_string(),
            r#"{"type":"content_block_start","index":1,"content_block":{"type":"text","text":""}}"#.to_string(),
            r#"{"type":"content_block_delta","index":1,"delta":{"type":"text_delta","text":"the "}}"#.to_string(),
            r#"{"type":"content_block_delta","index":1,"delta":{"type":"text_delta","text":"account"}}"#.to_string(),
            r#"{"type":"content_block_stop","index":1}"#.to_string(),
            r#"{"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":1}}"#.to_string(),
            r#"{"type":"message_stop"}"#.to_string(),
        ]))
    });
    // The compaction loop runs on the session's own client; the test starts it directly,
    // without a runtime around it.
    let endpoint = crate::api::Endpoint {
        provider: crate::api::Provider::Anthropic,
        url: Some(format!("http://{addr}/v1/messages")),
        auth_header: "x-api-key".into(),
        auth_value: "k".into(),
        session_id: None,
        extra_headers: Vec::new(),
        request: Default::default(),
        base: None,
    };
    let client = Arc::new(Client::new(endpoint, "m", "sys", None));
    let (session, _rx) = Session::new(std::env::current_dir().unwrap(), Log::in_memory());
    tokio::spawn(crate::daemon::compact::compact_loop(
        client,
        Arc::clone(&session),
        Vec::new(),
    ));
    append_compactable_history(&session);
    let mut deltas = session.subscribe_stream();

    assert_eq!(session.request_compaction(true), Ok(()));
    wait_until("the compaction finishes", || !session.compacting()).await;

    let mut seen = Vec::new();
    while let Ok(delta) = deltas.try_recv() {
        seen.push(match delta {
            StreamDelta::CompactionThinking { text } => format!("thinking:{text}"),
            StreamDelta::CompactionText { text } => format!("text:{text}"),
            StreamDelta::AssistantText { .. } | StreamDelta::ThinkingText { .. } => {
                panic!("a writer's fragment arrived as the main response's")
            }
            _ => continue,
        });
    }
    assert_eq!(seen, ["thinking:weighing it", "text:the ", "text:account"]);
}

/// A connection whose capabilities name no prompt cache still writes a compaction asked
/// for with `warm: true` (docs/summary.md). The request is accepted without a cold
/// belief, so the worker has to exist whatever the capabilities say; the cache-expiry
/// machinery (the cache lifetime the cold watcher reads, the keepalive) stays off.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_warm_compaction_runs_on_a_connection_without_a_prompt_cache() {
    use super::upstream_hold::{Answer, end_turn, scripted, start, wait_until};
    let requests = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let addr = scripted(Arc::clone(&requests), |_| Answer::Stream(end_turn()));
    let (host, runtime) = start(addr);
    let session = Arc::clone(runtime.session());
    wait_until("the loop went idle", || session.loop_idled()).await;
    assert!(
        !crate::api::ModelCapabilities::unknown()
            .prompt_cache
            .is_supported(),
        "the boot's capabilities name no prompt cache"
    );
    assert!(
        session.cache_ttl.get().is_none(),
        "no cache lifetime is recorded, so the cold watcher and keepalive were not started"
    );

    // The compaction names the model it was written by, from the boot's preamble.
    session
        .append(
            0,
            Payload::preamble(
                "",
                "sys",
                Default::default(),
                Some(crate::log::LoggedInference::new(
                    "c",
                    crate::api::Provider::Anthropic,
                    "m",
                )),
            ),
        )
        .unwrap();
    append_compactable_history(&session);
    assert_eq!(
        session.request_compaction(false),
        Err(NotCompactable::NotCold)
    );
    assert_eq!(session.request_compaction(true), Ok(()));

    wait_until("the compaction finishes", || !session.compacting()).await;
    let entries = session.snapshot();
    let summaries: Vec<&Summary> = entries
        .iter()
        .filter_map(|e| match &e.payload {
            Payload::Summary(summary) => Some(summary),
            _ => None,
        })
        .collect();
    assert_eq!(summaries.len(), 1, "the compaction ends in its Summary");
    assert!(matches!(
        summaries[0].made_by,
        crate::log::MadeBy::Compaction(_)
    ));
    assert!(
        summaries[0].text.ends_with("ok"),
        "the writer's account follows the header"
    );
    assert!(
        !entries.iter().any(|e| matches!(
            e.payload,
            Payload::Telemetry(crate::log::Telemetry::CompactionFailed { .. })
        )),
        "the compaction did not fail"
    );
    assert_eq!(
        requests.load(Ordering::SeqCst),
        1,
        "the compaction's request is the only one the upstream saw"
    );
    assert!(session.cache_ttl.get().is_none());
    host.shut_down().await;
}
