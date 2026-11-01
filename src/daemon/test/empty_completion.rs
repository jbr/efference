//! A completion with neither prose nor a call is asked again (docs/transport.md): kept
//! record-only as an empty completion, and the next request re-sends the same bytes.

use super::upstream_hold::start;
use super::*;
use crate::inference::testing::upstream::{Canned, ScriptedUpstream, end_turn, events};
use crate::record::log::Exclusion;
use crate::session::submitted::SubmittedTurn;

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

#[tokio::test(start_paused = true)]
async fn a_thinking_only_completion_is_asked_again() {
    let upstream = ScriptedUpstream::new(|n| match n {
        0 => Canned::stream(thinking_only()),
        _ => Canned::stream(end_turn()),
    });
    let (_host, runtime, _dir) = start(&upstream).await;
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
    // The upstream answers title asks aside, so every recorded request is the turn's.
    let turns: Vec<String> = upstream.requests().into_iter().map(|r| r.body).collect();
    assert!(turns.len() >= 2, "asked again");
    assert_eq!(
        turns[0], turns[1],
        "the second request re-sends the same bytes"
    );
}
