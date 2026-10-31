//! Tests for how a degenerate stream is classified: retryable marking,
//! frame capture and the captured-header allowlist, a death before
//! finalizing, and an empty turn that was billed.

use super::*;

/// A stream that **finalizes with nothing** is marked retryable, the complement
/// of one that dies before finalizing (docs/transport.md): proper termination,
/// zero content blocks, zero usage. Unmarked it commits as the model choosing
/// silence, so the loop has
/// nothing to dispatch and idles with the task unfinished. Marked, the turn loop
/// and the triage each retry it on the budget they already have.
#[test]
fn a_stream_that_finalizes_empty_is_marked_retryable() {
    let completion = feed(&[
        (
            "message_start",
            r#"{"message":{"model":"google/gemini-3.7-flash","usage":{"input_tokens":0,"output_tokens":0}}}"#,
        ),
        ("message_stop", "{}"),
    ])
    .unwrap();
    let why = completion.upstream_failure().expect("must be marked");
    assert!(why.contains("no content"), "{why}");
}

/// A stream that finalizes carrying a tool input the fragments do not spell as
/// valid JSON is marked retryable too (docs/transport.md), not returned as an
/// error — a foreign model can send `message_stop` after `input_json_delta`
/// fragments that end mid-string. As an error it leaves the
/// abort-retry budget unspent and puts the serde message where the model could
/// read it; marked, it is a record-only partial the loop retries, and the raw
/// fragment survives as a string — the entry is never projected or dispatched,
/// and may be the only copy of it.
#[test]
fn a_finalized_stream_with_unparseable_tool_input_is_marked_retryable() {
    let completion = feed(&[
        (
            "message_start",
            r#"{"message":{"model":"z-ai/glm-5.3","usage":{"input_tokens":9,"output_tokens":1}}}"#,
        ),
        (
            "content_block_start",
            r#"{"index":0,"content_block":{"type":"tool_use","id":"toolu_9","name":"bash"}}"#,
        ),
        (
            "content_block_delta",
            r#"{"index":0,"delta":{"type":"input_json_delta","partial_json":"{\"command\": \"echo cut"}}"#,
        ),
        ("content_block_stop", r#"{"index":0}"#),
        (
            "message_delta",
            r#"{"delta":{"stop_reason":"tool_use"},"usage":{"output_tokens":7}}"#,
        ),
        ("message_stop", r#"{"type":"message_stop"}"#),
    ])
    .expect("a malformed tool input is a marked completion, not an error");
    let why = completion.upstream_failure().expect("must be marked");
    assert!(why.contains("unparseable tool input"), "{why}");
    match &completion.assistant {
        Payload::Assistant { output, .. } => assert_eq!(
            output.tool_calls().unwrap()[0].input,
            Value::String(r#"{"command": "echo cut"#.to_string()),
            "the raw fragment is kept verbatim"
        ),
        other => panic!("expected Payload::Assistant, got {other:?}"),
    }
}

/// The empty stream is the one path that keeps evidence (docs/transport.md).
/// Nothing else survives it — the generation id it allocates 404s at OpenRouter
/// on both `GET /generation` and `GET /generation/content` — so the record-only
/// entry carries the response status, the allowlisted response headers (the
/// edge's own `x-generation-id`), the `message_start` body as received, and the
/// frames that arrived.
#[test]
fn an_empty_stream_records_status_headers_and_frames() {
    let mut a = Assembler::new();
    a.observed_response(ResponseMeta {
        status: 200,
        headers: BTreeMap::from([
            ("x-generation-id".to_string(), "gen-abc123".to_string()),
            ("content-type".to_string(), "text/event-stream".to_string()),
        ]),
    });
    let message_start = r#"{"message":{"id":"gen-abc123","model":"m","provider":"Google AI Studio","usage":{"input_tokens":0,"output_tokens":0}}}"#;
    a.apply(Some("message_start"), message_start).unwrap();
    a.apply(Some("data"), "[DONE]").unwrap();
    let completion = a.finish().unwrap();

    let Payload::Assistant {
        excluded: Some(Exclusion::Upstream(failure)),
        ..
    } = &completion.assistant
    else {
        panic!("an empty stream is a record-only entry")
    };
    let stream = failure.stream.as_ref().expect("diagnostics are attached");
    assert_eq!(stream.status, 200);
    assert_eq!(
        stream.headers.get("x-generation-id").map(String::as_str),
        Some("gen-abc123")
    );
    // Kept whole, so a field the assembler does not model — OpenRouter's
    // `provider`, naming the serving leg — reaches the log anyway.
    assert_eq!(stream.message_start.as_deref(), Some(message_start));
    assert!(stream.frames.contains("event: message_start"), "{stream:?}");
    assert!(stream.frames.contains("data: [DONE]"), "{stream:?}");
    assert!(!stream.frames_truncated);
}

/// A response header outside [`CAPTURED_HEADERS`] never enters the record. The
/// allowlist is the guard against an edge attaching an account-scoped header the
/// log then starts keeping (docs/env.md).
#[test]
fn captured_headers_are_an_allowlist() {
    assert!(is_captured_header("X-Generation-Id"));
    assert!(is_captured_header("content-type"));
    assert!(!is_captured_header("set-cookie"));
    assert!(!is_captured_header("authorization"));
}

/// The capture is bounded. An empty stream is a few hundred bytes by definition,
/// so the cap only ever fires on a stream this record is not for: the head is
/// kept, the rest dropped, and the entry says which.
#[test]
fn frame_capture_stops_at_the_cap() {
    let mut a = Assembler::new();
    let filler = "x".repeat(512);
    for _ in 0..64 {
        a.apply(Some("ping"), &format!("{{\"pad\":\"{filler}\"}}"))
            .unwrap();
    }
    a.apply(
        Some("message_start"),
        r#"{"message":{"model":"m","usage":{"input_tokens":0,"output_tokens":0}}}"#,
    )
    .unwrap();
    a.apply(Some("data"), "[DONE]").unwrap();
    let completion = a.finish().unwrap();

    let Payload::Assistant {
        excluded: Some(Exclusion::Upstream(failure)),
        ..
    } = &completion.assistant
    else {
        panic!("an empty stream is a record-only entry")
    };
    let stream = failure.stream.as_ref().expect("diagnostics are attached");
    assert!(stream.frames_truncated);
    assert!(
        stream.frames.len() <= FRAME_CAPTURE_CAP,
        "{}",
        stream.frames.len()
    );
}

/// Every other failure path stays the bare prose it was: a stream that dies
/// before finalizing has its partial, which the diagnostics do not add to, and
/// the entry serializes to the same bytes older logs wrote.
#[test]
fn a_death_before_finalizing_carries_no_diagnostics() {
    let completion = feed(&[(
        "message_start",
        r#"{"message":{"model":"m","usage":{"input_tokens":3,"output_tokens":0}}}"#,
    )])
    .unwrap();
    let Payload::Assistant {
        excluded: Some(Exclusion::Upstream(failure)),
        ..
    } = &completion.assistant
    else {
        panic!("an unfinalized stream is a record-only entry")
    };
    assert!(failure.stream.is_none());
    assert_eq!(
        serde_json::to_value(failure).unwrap(),
        serde_json::json!("stream ended before finalizing (no message_stop)")
    );
}

/// …but **only** with zero usage to corroborate. A turn billed for output that
/// nonetheless carries no blocks is the model's own answer, not a gateway
/// swallowing one, so it is not an upstream failure. It does say nothing, which the
/// turn loop asks again for as an empty completion (docs/transport.md).
#[test]
fn an_empty_turn_that_was_billed_is_left_alone() {
    let completion = feed(&[
        (
            "message_start",
            r#"{"message":{"model":"m","usage":{"input_tokens":12,"output_tokens":7}}}"#,
        ),
        ("message_stop", "{}"),
    ])
    .unwrap();
    assert!(
        completion.upstream_failure().is_none(),
        "a billed turn is not an empty stream"
    );
    assert!(completion.says_nothing());
}

/// A finalized turn says nothing when it has neither prose nor a call: thinking alone,
/// or whitespace text. A call, an `ask` included, is something; so is a turn the output
/// cap cut, which asked again would meet the same cap (docs/transport.md).
#[test]
fn a_turn_says_nothing_without_prose_or_a_call() {
    let start = (
        "message_start",
        r#"{"message":{"model":"m","usage":{"input_tokens":12,"output_tokens":7}}}"#,
    );
    let ended = |stop: &'static str| {
        [
            ("content_block_stop", r#"{"index":0}"#),
            ("message_delta", stop),
            ("message_stop", "{}"),
        ]
    };
    let end_turn = r#"{"delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":7}}"#;
    let max_tokens = r#"{"delta":{"stop_reason":"max_tokens"},"usage":{"output_tokens":7}}"#;
    let tool_use = r#"{"delta":{"stop_reason":"tool_use"},"usage":{"output_tokens":7}}"#;
    let thinking = [
        start,
        (
            "content_block_start",
            r#"{"index":0,"content_block":{"type":"thinking","thinking":""}}"#,
        ),
        (
            "content_block_delta",
            r#"{"index":0,"delta":{"type":"thinking_delta","thinking":"hmm"}}"#,
        ),
    ];
    let whitespace = [
        start,
        (
            "content_block_start",
            r#"{"index":0,"content_block":{"type":"text","text":""}}"#,
        ),
        (
            "content_block_delta",
            r#"{"index":0,"delta":{"type":"text_delta","text":"  \n"}}"#,
        ),
    ];
    let asking = [
        start,
        (
            "content_block_start",
            r#"{"index":0,"content_block":{"type":"tool_use","id":"toolu_1","name":"ask","input":{}}}"#,
        ),
        (
            "content_block_delta",
            r#"{"index":0,"delta":{"type":"input_json_delta","partial_json":"{\"question\":\"ok?\"}"}}"#,
        ),
    ];
    let says_nothing = |head: &[(&'static str, &'static str)], stop| {
        let events: Vec<_> = head.iter().copied().chain(ended(stop)).collect();
        feed(&events).unwrap().says_nothing()
    };
    assert!(says_nothing(&thinking, end_turn));
    assert!(says_nothing(&whitespace, end_turn));
    assert!(!says_nothing(&asking, tool_use));
    assert!(!says_nothing(&thinking, max_tokens));
}
