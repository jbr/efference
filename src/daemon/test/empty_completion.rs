//! A completion with neither prose nor a call is asked again (docs/transport.md): kept
//! record-only as an empty completion, and the next request re-sends the same bytes.

use super::upstream_hold::{Answer, end_turn, events, is_title_ask, recording, start};
use super::*;
use crate::frontend::SubmittedTurn;
use crate::log::Exclusion;
use std::sync::atomic::AtomicUsize;

/// A finalized turn that only thinks.
fn thinking_only() -> String {
    events(&[
        r#"{"type":"message_start","message":{"model":"m","usage":{"input_tokens":1,"output_tokens":0}}}"#.to_string(),
        r#"{"type":"content_block_start","index":0,"content_block":{"type":"thinking","thinking":""}}"#.to_string(),
        r#"{"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"pondering"}}"#.to_string(),
        r#"{"type":"content_block_stop","index":0}"#.to_string(),
        r#"{"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":5}}"#.to_string(),
        r#"{"type":"message_stop"}"#.to_string(),
    ])
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_thinking_only_completion_is_asked_again() {
    let requests = Arc::new(AtomicUsize::new(0));
    let bodies = Arc::new(Mutex::new(Vec::new()));
    let addr = recording(
        Arc::clone(&requests),
        |n| match n {
            0 => Answer::Stream(thinking_only()),
            _ => Answer::Stream(end_turn()),
        },
        &bodies,
    );
    let (_host, runtime) = start(addr);
    let session = Arc::clone(runtime.session());
    session.submit_turn(SubmittedTurn::new("go", "s1"));

    let closed = |session: &Session| {
        session.snapshot().iter().any(|e| {
            matches!(&e.payload, Payload::Assistant { output, excluded: None }
                if output.text() == ["ok"])
        })
    };
    session
        .wait_until("the turn closes with prose", || closed(&session))
        .await;

    let excluded: Vec<_> = session
        .snapshot()
        .into_iter()
        .filter_map(|e| match e.payload {
            Payload::Assistant {
                excluded: Some(why),
                ..
            } => Some(why),
            _ => None,
        })
        .collect();
    assert_eq!(
        excluded,
        vec![Exclusion::Empty],
        "kept as an empty completion"
    );
    let turns: Vec<String> = bodies
        .lock()
        .unwrap()
        .iter()
        .filter(|body| !is_title_ask(body))
        .cloned()
        .collect();
    assert!(turns.len() >= 2, "asked again");
    assert_eq!(
        turns[0], turns[1],
        "the second request re-sends the same bytes"
    );
}
