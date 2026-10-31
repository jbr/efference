//! Tests for upstream failure counting and clearing, the retry topic, the
//! bounded attempt queue, and the open-failure statuses (400, 429, 503).

use super::*;
use crate::testing::upstream::{Canned, ScriptedUpstream};

/// Consecutive equal statuses increment the diagnostic counter; a different
/// status starts a new run (docs/transport.md).
#[test]
fn upstream_failure_counts_consecutive_statuses() {
    let failure = upstream_failure_state();

    record_upstream_failure(&failure, Some(429), "");
    record_upstream_failure(&failure, Some(429), "");

    assert_eq!(
        *failure.lock().unwrap(),
        Some(UpstreamFailure {
            status: Some(429),
            count: 2,
            detail: String::new(),
        })
    );
    record_upstream_failure(&failure, Some(503), "");
    assert_eq!(
        *failure.lock().unwrap(),
        Some(UpstreamFailure {
            status: Some(503),
            count: 1,
            detail: String::new(),
        })
    );
}

#[test]
fn latest_failure_replaces_detail_and_a_stream_can_fail_after_recovery() {
    let failure = upstream_failure_state();
    record_upstream_failure(&failure, Some(503), "first");
    record_upstream_failure(&failure, Some(503), "second");
    let latest = failure.lock().unwrap().clone().unwrap();
    assert_eq!(latest.count, 2);
    assert_eq!(latest.detail, "second");
    client::clear_upstream_failure(&failure);
    record_upstream_failure(&failure, None, "stream closed");
    assert_eq!(
        *failure.lock().unwrap(),
        Some(UpstreamFailure::new(None, "stream closed"))
    );
}

/// The chip's all-clear: a 2xx clears the diagnostic, so a recovered session stops
/// reporting a failure that is over (docs/transport.md).
#[test]
fn upstream_failure_clears_on_recovery() {
    let failure = upstream_failure_state();

    record_upstream_failure(&failure, Some(429), "");
    assert!(failure.lock().unwrap().is_some());

    client::clear_upstream_failure(&failure);
    assert_eq!(*failure.lock().unwrap(), None);

    // A later failure starts a fresh run rather than resuming the cleared count.
    record_upstream_failure(&failure, Some(429), "");
    assert_eq!(
        *failure.lock().unwrap(),
        Some(UpstreamFailure {
            status: Some(429),
            count: 1,
            detail: String::new(),
        })
    );
}

/// One abort-retry-finalize cycle on the turn surface: the topic appears when the
/// first attempt aborts, tracks the attempt number, and is cleared by the attempt
/// that finalizes (docs/transport.md).
#[test]
fn retry_topic_tracks_attempts_and_clears_on_finalize() {
    let retry = upstream_retry_state();
    assert_eq!(retry.get(), None);

    for attempt in 1..=3u32 {
        retry.set(crate::agent::stream_abort_retry(
            RetrySurface::Turn,
            attempt,
            crate::agent::STREAM_ABORT_ATTEMPTS,
            std::time::Duration::from_secs(1),
            "upstream SSE stream failed mid-stream: connection reset",
        ));
        let live = retry.get().expect("a retry is in flight");
        assert_eq!(live.surface, RetrySurface::Turn);
        assert_eq!(live.attempt, attempt);
        assert_eq!(live.budget, crate::agent::STREAM_ABORT_ATTEMPTS);
        // Leads with what is happening, not with a status code.
        assert_eq!(
            live.message,
            format!(
                "attempt {attempt} of {} failed upstream, retrying",
                crate::agent::STREAM_ABORT_ATTEMPTS
            )
        );
        assert!(live.detail.contains("connection reset"));
        assert!(live.next_attempt_at > crate::log::Timestamp::now().0 - 1000);
    }

    retry.clear(RetrySurface::Turn);
    assert_eq!(retry.get(), None);
}

/// Exhaustion is not a state the topic holds: the budget running out leaves it
/// cleared, and the outcome is visible through the notification and the
/// record-only entries instead (docs/transport.md).
#[test]
fn exhaustion_leaves_the_retry_topic_cleared() {
    let retry = upstream_retry_state();
    retry.set(crate::agent::stream_abort_retry(
        RetrySurface::Triage,
        crate::agent::STREAM_ABORT_ATTEMPTS - 1,
        crate::agent::STREAM_ABORT_ATTEMPTS,
        std::time::Duration::from_secs(30),
        "aborted",
    ));
    assert!(retry.get().is_some());

    // The loop's exit on a spent budget: no pause to publish, only the clear.
    assert_eq!(
        crate::agent::stream_abort_backoff(crate::agent::STREAM_ABORT_ATTEMPTS),
        None
    );
    retry.clear(RetrySurface::Triage);
    assert_eq!(retry.get(), None);
}

/// The clear is scoped to its surface, so one loop's all-clear cannot erase the
/// other's live retry — and the two surfaces stay distinguishable, which is the
/// asymmetry that motivated giving the triage its own budget (docs/transport.md).
#[test]
fn a_triage_clear_does_not_erase_a_turn_retry() {
    let retry = upstream_retry_state();
    retry.set(crate::agent::stream_abort_retry(
        RetrySurface::Turn,
        2,
        crate::agent::STREAM_ABORT_ATTEMPTS,
        std::time::Duration::from_secs(2),
        "aborted",
    ));

    retry.clear(RetrySurface::Triage);

    assert_eq!(
        retry.get().map(|r| r.surface),
        Some(RetrySurface::Turn),
        "the turn's retry survives a triage's all-clear"
    );
}

/// Failed attempts queue in arrival order and drain exactly once — the drain is
/// what hands the loop the bodies it turns into durable records
/// (docs/transport.md).
#[test]
fn absorbed_attempts_drain_once_in_order() {
    let queue = upstream_attempt_queue();
    for status in [429u16, 503] {
        push_upstream_attempt(
            &queue,
            UpstreamAttemptFailure {
                status,
                body: format!("{{\"error\":{{\"message\":\"{status} says so\"}}}}"),
            },
        );
    }

    let drained = drain_upstream_attempts(&queue);
    assert_eq!(
        drained.iter().map(|f| f.status).collect::<Vec<_>>(),
        [429, 503]
    );
    assert_eq!(
        drained[0].record_text(),
        "upstream attempt failed (HTTP 429): {\"error\":{\"message\":\"429 says so\"}}"
    );
    assert!(
        drain_upstream_attempts(&queue).is_empty(),
        "a drained queue is empty — the record is durable in the log, not here"
    );
}

/// A queue nobody drains keeps the most recent failures and drops the oldest,
/// so a long outage cannot grow the process without bound (docs/transport.md).
#[test]
fn the_attempt_queue_is_bounded() {
    let queue = upstream_attempt_queue();
    for n in 0..200u16 {
        push_upstream_attempt(
            &queue,
            UpstreamAttemptFailure {
                status: 503,
                body: n.to_string(),
            },
        );
    }
    let drained = drain_upstream_attempts(&queue);
    assert!(drained.len() < 200, "the queue is capped");
    assert_eq!(
        drained.last().expect("non-empty").body,
        "199",
        "the newest failure is the one kept"
    );
}

/// The transport's own retry gate reads the non-2xx body (docs/transport.md): a
/// 429 whose body names an exhausted plan is answered once, where the status
/// alone would be retried. The upstream counts the requests.
#[tokio::test]
async fn a_terminal_429_body_is_answered_once() {
    let terminal = r#"{"error":{"type":"usage_limit_reached","message":"The usage limit has been reached","plan_type":"plus","resets_at":1788744444,"resets_in_seconds":15805}}"#;
    let upstream = ScriptedUpstream::new(move |_| Canned::json(429, terminal));
    let client = Client::new(
        upstream.endpoint(Provider::OpenAICodex, "/backend-api/codex/responses"),
        "gpt-5.6-sol",
        "sys",
    );
    let outcome = client.complete(&[], &[], ToolChoice::auto(), |_| {}).await;
    assert!(outcome.is_err(), "a 429 that never clears is an error");
    assert_eq!(upstream.count(), 1, "an exhausted plan is answered once");
}

/// A non-2xx open outside the transient set is not retried: the body comes back
/// in the error and is queued as a durable attempt for the loop to record
/// (docs/transport.md).
#[tokio::test]
async fn a_400_open_returns_its_body_once_and_queues_the_attempt() {
    let body = r#"{"error":{"type":"invalid_request_error","code":"invalid_prompt","message":"bad input"}}"#;
    let upstream = ScriptedUpstream::new(move |_| Canned::json(400, body));
    let client = Client::new(
        upstream.endpoint(Provider::OpenAI, "/v1/responses"),
        "gpt-5.6-luna",
        "sys",
    );
    let error = client
        .complete(&[], &[], ToolChoice::auto(), |_| {})
        .await
        .expect_err("a 400 is an open failure");
    let diagnostic = client.upstream_failure().lock().unwrap().clone().unwrap();
    assert_eq!(diagnostic.status, Some(400));
    assert!(diagnostic.detail.contains("bad input"));
    assert_eq!(upstream.count(), 1, "not retried");
    let text = error.to_string();
    assert!(text.contains("400"), "{text}");
    assert!(text.contains("bad input"), "{text}");
    let attempts = client.drain_upstream_attempts();
    assert_eq!(attempts.len(), 1);
    assert_eq!(attempts[0].status, 400);
    assert!(attempts[0].body.contains("invalid_prompt"));
}

/// The documented transient family is retried by the transport itself: a 503
/// (`server_is_overloaded`) followed by a 200 completes, and the absorbed attempt
/// is queued with its body for the loop's record (docs/transport.md).
#[tokio::test(start_paused = true)]
async fn a_503_open_is_retried_and_the_attempt_is_queued() {
    let upstream = ScriptedUpstream::answering([
        Canned::json(
            503,
            r#"{"error":{"type":"service_unavailable_error","code":"server_is_overloaded","message":"busy"}}"#,
        ),
        Canned::stream(
            "event: response.completed\ndata: {\"type\":\"response.completed\",\
             \"response\":{\"id\":\"r\",\"status\":\"completed\",\"output\":[],\
             \"usage\":{\"input_tokens\":1,\"output_tokens\":1}}}\n\n",
        ),
    ]);
    let client = Client::new(
        upstream.endpoint(Provider::OpenAI, "/v1/responses"),
        "gpt-5.6-luna",
        "sys",
    );
    let completion = client
        .complete(&[], &[], ToolChoice::auto(), |_| {})
        .await
        .expect("the second attempt completes");
    assert_eq!(upstream.count(), 2);
    assert!(completion.upstream_failure().is_none());
    let attempts = client.drain_upstream_attempts();
    assert_eq!(attempts.len(), 1);
    assert_eq!(attempts[0].status, 503);
    assert!(attempts[0].body.contains("server_is_overloaded"));
}

/// The operator's "retry now" (docs/transport.md) ends a backoff pause early, and a
/// hurry landing before the pause starts is not lost; an unhurried pause runs out.
#[tokio::test(start_paused = true)]
async fn a_hurry_ends_the_pause_whenever_it_lands() {
    let retry = upstream_retry_state();
    let long = Some(std::time::Duration::from_secs(60));

    retry.hurry();
    assert!(retry.pause(long).await, "a hurry before the pause is kept");

    let waiting = std::sync::Arc::clone(&retry);
    let pause = tokio::spawn(async move { waiting.pause(long).await });
    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    retry.hurry();
    assert!(pause.await.unwrap(), "a hurry during the pause ends it");

    assert!(
        !retry
            .pause(Some(std::time::Duration::from_millis(10)))
            .await,
        "each hurry is consumed once"
    );
}
