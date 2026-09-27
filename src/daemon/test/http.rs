//! Tests for `http.rs`.

use super::*;

/// `PATCH /loop {review_level}` over the real router + control client (docs/permissions.md):
/// a valid level is set and echoed on `/state`; an unknown level is rejected (400) and the dial
/// is left untouched.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn review_level_over_the_router() {
    let log_path = std::env::temp_dir().join("eff-review-level.jsonl");
    let _ = std::fs::remove_file(&log_path);

    let (shared, _turns_rx) = Session::new(
        std::env::current_dir().unwrap(),
        Log::create(&log_path).expect("create log"),
    );
    let client = test_client(&shared).await;
    assert_eq!(
        client.review_level("human_only").await.unwrap(),
        "human_only"
    );
    let state: Value = serde_json::from_str(&client.state().await.unwrap()).unwrap();
    assert_eq!(state["review_level"], "human_only");
    // `autonomous` is not mirrored here as a derived bool: it is exactly
    // `review_level == "autonomous"`, which a client can compute, and a state topic
    // carries one value rather than a value plus a restatement of part of it
    // (docs/control-api.md).
    assert!(state.get("autonomous").is_none(), "not restated as a bool");

    // An unknown level is rejected in the typed envelope (docs/control-api.md) and does
    // not move the dial. Asserted through a route rather than on `ErrorBody` directly:
    // what a client depends on is that the *router* answers in one shape, which a test of
    // the type alone would keep passing through a handler that hand-rolled a body.
    let err = client
        .review_level("wat")
        .await
        .expect_err("unknown → error")
        .to_string();
    assert!(err.contains(r#""code":"bad_request""#), "typed code: {err}");
    assert!(err.contains(r#""message":"#), "human half: {err}");
    let state: Value = serde_json::from_str(&client.state().await.unwrap()).unwrap();
    assert_eq!(state["review_level"], "human_only", "unchanged after a 400");

    let _ = std::fs::remove_file(&log_path);
}

/// `POST /turns` submission ids (docs/control-api.md): a client-minted id is
/// adopted — echoed in the 202 body and carried on the queued turn — a body
/// without one gets a daemon-minted id, and an oversized one is rejected
/// without enqueueing anything.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn turn_submission_ids_adopt_or_mint() {
    let (shared, mut turns_rx) = Session::new(std::env::current_dir().unwrap(), Log::in_memory());
    let client = test_client(&shared).await;

    // Client-minted: adopted verbatim, echoed back, and on the queued turn.
    let body = client
        .turn_with_submission_id("hello", "sub-mine")
        .await
        .unwrap();
    assert!(
        body.contains(r#""submission_id":"sub-mine""#),
        "echo: {body}"
    );
    let queued = turns_rx.recv().await.unwrap();
    assert_eq!(queued.text, "hello");
    assert_eq!(queued.submission_id, "sub-mine");

    // Absent: daemon-minted, so every submitted turn is identified.
    client.turn("bare").await.unwrap();
    let queued = turns_rx.recv().await.unwrap();
    assert_eq!(queued.text, "bare");
    assert!(!queued.submission_id.is_empty(), "minted when absent");

    // Oversized: rejected in the typed envelope, nothing enqueued.
    let err = client
        .turn_with_submission_id("nope", &"x".repeat(129))
        .await
        .expect_err("oversized id → 400")
        .to_string();
    assert!(err.contains(r#""code":"bad_request""#), "typed code: {err}");
    assert!(turns_rx.try_recv().is_err(), "rejected turn never queued");
}

/// `POST /turns` carries an optional client-asserted `source` onto the queued
/// turn (docs/control-api.md); absent, the turn is the operator's and carries
/// none.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn turn_source_reaches_the_queued_turn() {
    let (shared, mut turns_rx) = Session::new(std::env::current_dir().unwrap(), Log::in_memory());
    let client = test_client(&shared).await;

    client
        .turn_sourced("hi from a script", Some("agent[abc]"))
        .await
        .unwrap();
    let queued = turns_rx.recv().await.unwrap();
    assert_eq!(queued.source.as_deref(), Some("agent[abc]"));

    client.turn("from the operator").await.unwrap();
    let queued = turns_rx.recv().await.unwrap();
    assert_eq!(queued.source, None);
}

/// The kickoff's source is the harness's alone: the clients leave a turn carrying
/// it out of the transcript, so a client claiming it is refused (docs/context.md).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_kickoff_source_is_reserved() {
    let (shared, mut turns_rx) = Session::new(std::env::current_dir().unwrap(), Log::in_memory());
    let client = test_client(&shared).await;
    let err = client
        .turn_sourced("hidden", Some(crate::frontend::KICKOFF_SOURCE))
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("reserved"), "{err}");
    let err = client
        .turn_sourced("hidden", Some("handoff[someone]"))
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("hand-off brief"), "{err}");
    assert!(turns_rx.try_recv().is_err(), "rejected turn never queued");
}

/// Recall over the wire (docs/control-api.md): `DELETE /turns/:submission_id` pops the suffix
/// and answers with what it actually popped, the `queued` state topic reflects the remainder,
/// and an id the loop already consumed pops nothing rather than erroring.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn withdraw_turns_pops_a_suffix_and_reports_what_it_popped() {
    let (shared, _turns_rx) = Session::new(std::env::current_dir().unwrap(), Log::in_memory());
    let client = test_client(&shared).await;

    client.turn_with_submission_id("one", "s1").await.unwrap();
    client.turn_with_submission_id("two", "s2").await.unwrap();
    client.turn_with_submission_id("three", "s3").await.unwrap();

    let state = client.state().await.unwrap();
    assert!(
        state.contains(r#""queued":[{"submission_id":"s1","text":"one"}"#),
        "the queue is readable in /state: {state}"
    );

    let popped = client.withdraw_turns("s2").await.unwrap();
    assert_eq!(
        popped.iter().map(|t| t.text.as_str()).collect::<Vec<_>>(),
        ["two", "three"]
    );
    let state = client.state().await.unwrap();
    assert!(
        state.contains(r#""queued":[{"submission_id":"s1","text":"one"}]"#),
        "the suffix is gone: {state}"
    );

    // The lost race: nothing left under that id — an empty pop, not an error.
    assert!(client.withdraw_turns("s2").await.unwrap().is_empty());
}

/// A `config_edit` decision body decodes into its change, the wire shape the orchestrator
/// posts to upgrade a gate (docs/permissions.md). A `lifetime` from an older
/// client is ignored rather than rejected — every committed change is written to the file now,
/// so there is no lifetime left to choose.
#[test]
fn config_edit_decision_body_decodes() {
    let body = r#"{"decision":"config_edit","lifetime":"persist","note":"in scope",
                       "change":{"command":{"command":"cargo nextest"}}}"#;
    let d: DecisionBody = serde_json::from_str(body).unwrap();
    assert_eq!(d.decision, "config_edit");
    assert_eq!(
        d.change,
        Some(crate::log::ConfigChange::Command {
            command: "cargo nextest".into()
        })
    );
}

/// `PUT /approvals/:call_id` treats the id as a precondition (docs/control-api.md):
/// addressing anything but the pending gate is 409 `gate_moved`, whose `detail`
/// carries the *current* gate (`null` = none) so a client re-renders without a
/// second round-trip. The old ambiguous 404 is gone.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn put_approvals_enforces_the_gate_precondition() {
    let (shared, _turns_rx) = Session::new(std::env::current_dir().unwrap(), Log::in_memory());
    let app = test_server(&shared).await;

    // No gate at all: the precondition fails, and the detail says "none pending".
    let conn = app
        .put("/v1/approvals/toolu_x")
        .with_body(r#"{"decision":"approve"}"#)
        .await;
    conn.assert_status(409);
    let v: Value = serde_json::from_str(conn.body()).unwrap();
    assert_eq!(v["code"], "gate_moved");
    assert!(v["detail"].is_null(), "no gate pending: {v}");

    // A gate for toolu_x: addressing a different id is the moved-gate race, and the
    // body hands back the gate that *is* pending.
    let _rx = shared.register_pending(&bash_call("cargo test"), None);
    let conn = app
        .put("/v1/approvals/toolu_stale")
        .with_body(r#"{"decision":"approve"}"#)
        .await;
    conn.assert_status(409);
    let v: Value = serde_json::from_str(conn.body()).unwrap();
    assert_eq!(
        v["detail"]["call_id"], "toolu_x",
        "the current gate rides as corrective detail: {v}"
    );

    // The precondition holds: the decision applies.
    let conn = app
        .put("/v1/approvals/toolu_x")
        .with_body(r#"{"decision":"approve"}"#)
        .await;
    conn.assert_status(200);
}

/// `GET`/`PATCH /loop` (docs/control-channel.md): one resource for the operator
/// dials, one call can move both — which the two verb-routes it replaced could not —
/// and a part-bad request moves nothing.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn loop_resource_moves_both_dials_in_one_call() {
    let (shared, _turns_rx) = Session::new(std::env::current_dir().unwrap(), Log::in_memory());
    let app = test_server(&shared).await;

    let read: Value = serde_json::from_str(app.get("/v1/loop").await.body()).unwrap();
    assert!(read["pace"]["speed_ms"].is_u64());
    assert_eq!(read["running"], false);

    let conn = app
        .patch("/v1/loop")
        .with_body(r#"{"pace":{"speed_ms":5000,"held":true},"review_level":"human_only"}"#)
        .await;
    conn.assert_status(200);
    let v: Value = serde_json::from_str(conn.body()).unwrap();
    assert_eq!(v["pace"]["speed_ms"], 5000);
    assert_eq!(v["pace"]["held"], true);
    assert_eq!(v["review_level"], "human_only");

    // Validated whole before anything is applied: a bad review_level rejects the
    // pace change riding beside it.
    app.patch("/v1/loop")
        .with_body(r#"{"pace":{"speed_ms":1},"review_level":"wat"}"#)
        .await
        .assert_status(400);
    let read: Value = serde_json::from_str(app.get("/v1/loop").await.body()).unwrap();
    assert_eq!(read["pace"]["speed_ms"], 5000, "nothing moved on a 400");

    // The resource's fields are its state topics, byte for byte.
    let state: Value = serde_json::from_str(&shared.state_json()).unwrap();
    assert_eq!(state["pace"], read["pace"]);
    assert_eq!(state["review_level"], read["review_level"]);
    assert_eq!(state["running"], read["running"]);
}

/// `GET /state?wait=settled` blocks until a decision point — an idle daemon is
/// already settled, so it answers at once with the snapshot — and the `wait`
/// grammar rejects a missing or unknown predicate with a 400, never silently
/// answering "now": a client that asked to wait must not be told the present is
/// what it waited for. (The predicate's lifecycle itself is pinned in `pace.rs`
/// at the `Session` level.)
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn state_wait_takes_a_predicate() {
    let (shared, _turns_rx) = Session::new(std::env::current_dir().unwrap(), Log::in_memory());
    let app = test_server(&shared).await;

    let conn = app.get("/v1/state?wait=settled").await;
    conn.assert_status(200);
    let v: Value = serde_json::from_str(conn.body()).unwrap();
    assert!(
        v.get("gate").is_some(),
        "the settled answer is a /state snapshot"
    );

    app.get("/v1/state?wait=now").await.assert_status(400);
    app.get("/v1/state?wait").await.assert_status(400);
    // And an un-waited read still answers immediately.
    app.get("/v1/state").await.assert_status(200);
}
