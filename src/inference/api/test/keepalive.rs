//! Scripted-stream coverage for the one mark a keepalive ping returns usage
//! for: the output cap cutting a tool call (docs/transport.md).

use super::*;
use crate::inference::testing::upstream::{Canned, ScriptedUpstream};
use crate::record::log::RequestOrigin;

/// The Codex route an upstream answers on.
const CODEX: &str = "/backend-api/codex/responses";

/// A Responses stream the output cap cuts mid-tool-call, with the arguments half
/// out — the derived view refuses them, the record keeps them.
const CUT: &str = concat!(
    "event: response.incomplete\n",
    r#"data: {"type":"response.incomplete","response":{"id":"r","status":"incomplete","incomplete_details":{"reason":"max_output_tokens"},"output":[{"type":"function_call","call_id":"c","name":"bash","arguments":"{\"command\":\""}],"usage":{"input_tokens":10,"output_tokens":5}}}"#,
    "\n\n",
);

/// The same cut, with the arguments whole enough to parse.
const CUT_WHOLE: &str = concat!(
    "event: response.incomplete\n",
    r#"data: {"type":"response.incomplete","response":{"id":"r","status":"incomplete","incomplete_details":{"reason":"max_output_tokens"},"output":[{"type":"function_call","call_id":"c","name":"bash","arguments":"{\"command\":\"ls\"}"}],"usage":{"input_tokens":10,"output_tokens":5}}}"#,
    "\n\n",
);

/// A `response.failed` answer carrying usage and a call — the cap had no part in it.
const FAILED: &str = concat!(
    "event: response.failed\n",
    r#"data: {"type":"response.failed","response":{"id":"r","status":"failed","output":[{"type":"function_call","call_id":"c","name":"bash","arguments":"{\"command\":\"ls\"}"}],"usage":{"input_tokens":10,"output_tokens":5}}}"#,
    "\n\n",
);

/// A stream that carries a call and its usage and then ends with no terminal event.
const NO_TERMINAL: &str = concat!(
    "event: response.created\n",
    r#"data: {"type":"response.created","response":{"id":"r","status":"in_progress","output":[{"type":"function_call","call_id":"c","name":"bash","arguments":"{\"command\":\"ls\"}"}],"usage":{"input_tokens":10,"output_tokens":5}}}"#,
    "\n\n",
);

/// A ping on the Codex route: the turn's own envelope, empty here, over the client's
/// prefix.
async fn ping(client: &Client) -> Result<RequestUsage, KeepaliveError> {
    client
        .keepalive(
            RequestOrigin::Keepalive,
            &SentRequest::new(Vec::new(), Vec::new(), ToolChoice::auto()),
        )
        .await
}

/// A Codex client over an upstream scripted with `answer`, which the client's
/// endpoint holds for as long as the returned upstream lives.
fn codex_client(answer: Canned) -> (Client, ScriptedUpstream) {
    let upstream = ScriptedUpstream::answering([answer]);
    let client = Client::new(
        upstream.endpoint(Provider::OpenAICodex, CODEX),
        "gpt-6.1-sol",
        "sys",
    );
    (client, upstream)
}

/// The cap's cut of a tool call is the one marked completion a ping returns usage
/// for: the cache touch is the point of the request, and the cut shows the prefix
/// was read.
#[tokio::test]
async fn a_ping_returns_usage_when_the_cap_cuts_a_tool_call() {
    let (client, _upstream) = codex_client(Canned::stream(CUT));
    let usage = ping(&client)
        .await
        .expect("the cap's cut of a tool call is accepted");
    assert_eq!(usage.usage.input_tokens, 10);
    assert_eq!(usage.usage.output_tokens, 5);
}

/// The same acceptance when the cut left arguments the derived view can parse: the
/// cap, not the parse, is the condition the acceptance checks.
#[tokio::test]
async fn a_ping_returns_usage_when_the_cap_cuts_a_call_whose_arguments_parse() {
    let (client, _upstream) = codex_client(Canned::stream(CUT_WHOLE));
    let usage = ping(&client)
        .await
        .expect("a parseable call cut by the cap is accepted");
    assert_eq!(usage.usage.input_tokens, 10);
}

/// A failed response carrying usage and a call still fails the ping: the cap is the
/// discriminator, and this answer carries no cap finish.
#[tokio::test]
async fn a_failed_response_still_fails_a_ping_that_carries_usage_and_a_call() {
    let (client, _upstream) = codex_client(Canned::stream(FAILED));
    let err = ping(&client)
        .await
        .expect_err("a response that failed is not the cap's cut");
    assert!(
        err.to_string().contains("upstream response failed"),
        "{err}"
    );
}

/// A stream that ends with no terminal event still fails the ping, call and usage
/// notwithstanding: nothing marked it as the cap's cut.
#[tokio::test]
async fn a_stream_with_no_terminal_event_still_fails_a_ping() {
    let (client, _upstream) = codex_client(Canned::stream(NO_TERMINAL));
    let err = ping(&client)
        .await
        .expect_err("a stream that never finalized is a failure");
    assert!(err.to_string().contains("finalizing"), "{err}");
}

/// The acceptance is the ping's alone: a real turn on the same streams reads the cut
/// as the failure it is — the arguments the cap cut, or the cap itself where they
/// parse — so tool-call validation is untouched (docs/transport.md).
#[tokio::test]
async fn a_turn_still_reads_the_cap_cut_as_a_failure() {
    let (client, _upstream) = codex_client(Canned::stream(CUT));
    let cut = client
        .complete(&[], &[], ToolChoice::auto(), |_| {})
        .await
        .expect("a marked completion is returned, not an error");
    assert!(
        cut.upstream_failure()
            .is_some_and(|why| why.contains("unparseable arguments")),
        "{:?}",
        cut.upstream_failure()
    );

    let (client, _upstream) = codex_client(Canned::stream(CUT_WHOLE));
    let whole = client
        .complete(&[], &[], ToolChoice::auto(), |_| {})
        .await
        .expect("a marked completion is returned, not an error");
    assert!(
        whole
            .upstream_failure()
            .is_some_and(|why| why.contains("output-token cap")),
        "{:?}",
        whole.upstream_failure()
    );
}
