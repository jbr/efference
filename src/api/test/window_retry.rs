//! Tests for the halve-and-retry on a window overflow refused inside a 200
//! stream (docs/transport.md), against an upstream that answers each request
//! from a script.

use super::*;
use crate::project::WireTurn;
use crate::testing::upstream::{Canned, ScriptedUpstream};

/// The refusal as DeepInfra sends it through OpenRouter: an `error` event on a stream
/// that opened 200, with no error type, so the classifier calls it transient.
const OVERFLOW_EVENT: &str = "event: error\ndata: {\"error\":{\"message\":\"This model's maximum \
    context length is 131072 tokens. However, you requested 65536 output tokens and your prompt \
    contains at least 65537 input tokens, for a total of at least 131073 tokens.\",\
    \"code\":400}}\n\n";

/// The same refusal named a terminal type, which is how the classifier marks it
/// terminal (`api/failure.rs`).
const TYPED_OVERFLOW_EVENT: &str = "event: error\ndata: {\"type\":\"error\",\"error\":{\"type\":\"invalid_request_error\",\"message\":\"This model's maximum \
    context length is 131072 tokens. However, you requested 65536 output tokens and your prompt \
    contains at least 65537 input tokens.\"}}\n\n";

/// A stream that finalizes with one line of text.
const ANSWER: &str = "event: message_start\n\
    data: {\"message\":{\"model\":\"m\",\"usage\":{\"input_tokens\":5,\"output_tokens\":0}}}\n\n\
    event: content_block_start\ndata: {\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n\
    event: content_block_delta\ndata: {\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"ok\"}}\n\n\
    event: content_block_stop\ndata: {\"index\":0}\n\n\
    event: message_delta\ndata: {\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":1}}\n\n\
    event: message_stop\ndata: {}\n\n";

/// An upstream that answers its nth request with the 200 event stream `script[n]`.
fn serve(script: Vec<&'static str>) -> ScriptedUpstream {
    ScriptedUpstream::answering(script.into_iter().map(Canned::stream))
}

/// The `max_tokens` each request `upstream` was sent carried, in arrival order.
fn reserved(upstream: &ScriptedUpstream) -> Vec<u64> {
    upstream
        .requests()
        .iter()
        .map(|request| request.json()["max_tokens"].as_u64().unwrap())
        .collect()
}

/// A Messages client against `upstream` with a 131072-token window and a 65536 output
/// cap.
fn client(upstream: &ScriptedUpstream) -> Client {
    Client::new(
        upstream.endpoint(Provider::OpenRouter, "/v1/messages"),
        "m",
        "sys",
        None,
    )
    .with_max_output(65_536)
    .with_context_window(Some(131_072))
}

/// A refusal inside a 200 stream halves the reservation and resends, as the 400 at
/// open does, whether or not the refusal names a terminal type: the answer to the
/// second request is the completion, and the refused attempt leaves nothing behind.
#[tokio::test]
async fn an_in_stream_window_overflow_is_resent_at_half_the_reservation() {
    let upstream = serve(vec![OVERFLOW_EVENT, ANSWER]);
    let client = client(&upstream);
    let turns = [WireTurn::text("hello")];
    let mut deltas = 0;
    let completion = client
        .complete(&turns, &[], ToolChoice::auto(), |_| deltas += 1)
        .await
        .expect("the second request completes");
    assert_eq!(reserved(&upstream), [65_536, 32_768]);
    assert!(completion.upstream_failure().is_none());
    assert_eq!(deltas, 1, "only the answer streamed");
}

/// The halving runs down to the floor and then stops: the last refusal is the
/// completion, terminal, with the provider's words.
#[tokio::test]
async fn an_in_stream_window_overflow_stops_at_the_floor() {
    let upstream = serve(vec![TYPED_OVERFLOW_EVENT; 7]);
    let client = client(&upstream);
    let turns = [WireTurn::text("hello")];
    let completion = client
        .complete(&turns, &[], ToolChoice::auto(), |_| {})
        .await
        .expect("an in-stream refusal is a marked completion");
    assert_eq!(
        reserved(&upstream),
        [65_536, 32_768, 16_384, 8_192, 4_096, 2_048, 1_024]
    );
    let why = completion
        .terminal_failure()
        .expect("terminal at the floor");
    assert!(why.contains("maximum context length"), "{why}");
}
