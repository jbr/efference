//! Tests for `fold.rs` (backlog: cold-session.md).

use super::*;
use crate::daemon::fold::{FOLD_TAIL_TOKENS, NotFoldable, fold_cut};
use crate::log::{AssistantOutput, LogEntry, Mark, RawResult, Timestamp, WireState};

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
            upstream_failure: None,
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
    assert_eq!(fold_cut(&log, 9_000), Some(2), "seqs 3 and 4 stay verbatim");
}

/// A cut that would separate a call from its result moves earlier.
#[test]
fn the_cut_does_not_split_a_call_from_its_result() {
    let log = [user(1, 5_000), call(2, "c1"), result(3, "c1", 9_000)];
    assert_eq!(
        fold_cut(&log, 9_000),
        Some(1),
        "the call goes with its result"
    );
}

/// Nothing to fold when everything fits, or when what is left was folded already.
#[test]
fn nothing_to_fold_when_the_tail_holds_it_all() {
    let log = [user(1, 100), user(2, 100)];
    assert_eq!(fold_cut(&log, FOLD_TAIL_TOKENS), None);

    let folded = [
        user(1, 5_000),
        user(2, 5_000),
        entry(3, Payload::Mark(Mark::boundary(2, "account"))),
        user(4, 9_000),
    ];
    assert_eq!(fold_cut(&folded, 9_000), None, "seq 2 is already covered");
}

/// After a fold the session is in the state of one that has sent nothing: not cold,
/// and nothing for the keepalive to ping.
#[tokio::test]
async fn a_fold_resets_the_cold_belief_and_the_keepalive() {
    let (shared, _rx) = Session::new(std::env::current_dir().unwrap(), Log::in_memory());
    shared.set_cache_ttl(Duration::ZERO);
    append_foldable_history(&shared);
    shared.record_request(
        RequestOrigin::Turn,
        crate::api::RequestUsage::new(crate::api::Usage::default()),
    );
    tokio::time::sleep(Duration::from_millis(5)).await;
    assert!(shared.cold_since().is_some());
    assert!(shared.keepalive_clocks().0.is_some());

    shared
        .append(1, Payload::Mark(Mark::boundary(0, "account")))
        .unwrap();
    assert_eq!(shared.cold_since(), None);
    assert_eq!(shared.keepalive_clocks().0, None);
}

/// A warm session has nothing to fold: the fold is the cold session's alternative to
/// an uncached read.
#[test]
fn a_fold_needs_a_cold_session() {
    let (shared, _rx) = Session::new(std::env::current_dir().unwrap(), Log::in_memory());
    assert_eq!(shared.request_fold(false), Err(NotFoldable::NotCold));
}

/// `warm: true` is the operator's explicit request to fold a session whose cache is
/// not believed expired; the progress shows on the `fold` topic, not the `cold` one.
#[tokio::test]
async fn a_warm_fold_is_accepted_on_request_and_reports_on_the_fold_topic() {
    let (shared, _rx) = Session::new(std::env::current_dir().unwrap(), Log::in_memory());
    shared.set_cache_ttl(Duration::from_secs(3_600));
    append_foldable_history(&shared);
    assert_eq!(shared.cold_since(), None);
    assert_eq!(shared.request_fold(false), Err(NotFoldable::NotCold));
    assert_eq!(shared.topic_json(Topic::Fold), Value::Null);

    assert_eq!(shared.request_fold(true), Ok(()));
    assert_eq!(
        shared.topic_json(Topic::Fold),
        serde_json::json!({"state": "writing"})
    );
    assert_eq!(shared.topic_json(Topic::Cold), Value::Null);
}

/// A loop held at a triage gate is running but sends nothing until the operator
/// retries, so it accepts a fold as a cold-parked loop does; one running a request
/// does not. The held gate's state was the `dry-linnet` case: a fold refused as
/// "the loop is running" left a cold read or an archive as the only ways forward.
#[test]
fn a_held_triage_gate_accepts_a_fold() {
    let (shared, _rx) = Session::new(std::env::current_dir().unwrap(), Log::in_memory());
    shared.set_cache_ttl(Duration::from_secs(3_600));
    append_foldable_history(&shared);
    shared
        .running
        .store(true, std::sync::atomic::Ordering::SeqCst);
    assert_eq!(shared.request_fold(true), Err(NotFoldable::Busy));

    shared.set_stall(crate::agent::stalled(
        StallReason::TriageUnavailable,
        None,
        "triage could not reach its model".into(),
        "429".into(),
    ));
    assert_eq!(shared.request_fold(true), Ok(()));
}

/// A warm fold with nothing past the tail still declines: warm lifts only the cache
/// guard.
#[test]
fn a_warm_fold_still_needs_something_to_fold() {
    let (shared, _rx) = Session::new(std::env::current_dir().unwrap(), Log::in_memory());
    shared.set_cache_ttl(Duration::from_secs(3_600));
    assert_eq!(shared.request_fold(true), Err(NotFoldable::Nothing));
}

/// A failed fold is reported until a turn starts, then dropped.
#[test]
fn a_failed_fold_clears_when_a_turn_starts() {
    let (shared, _rx) = Session::new(std::env::current_dir().unwrap(), Log::in_memory());
    *shared.fold_state.lock().unwrap() = Some(crate::daemon::fold::FoldState::Failed {
        error: "boom".into(),
    });
    assert_eq!(
        shared.topic_json(Topic::Fold),
        serde_json::json!({"state": "failed", "error": "boom"})
    );
    shared.take_continue();
    assert_eq!(shared.topic_json(Topic::Fold), Value::Null);
}

/// The client reads a refusal as a decline with the daemon's message, not an error.
#[tokio::test]
async fn the_client_reads_a_fold_refusal_as_a_decline() {
    let (shared, _rx) = Session::new(std::env::current_dir().unwrap(), Log::in_memory());
    let client = test_client(&shared).await;
    assert_eq!(
        client.fold().await.unwrap(),
        crate::client::FoldOutcome::Declined {
            message: NotFoldable::NotCold.message().into()
        }
    );
}
