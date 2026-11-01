//! The cache chain end to end (docs/transport.md): each turn names the response before
//! it, so the provider can compare two prompts and say what moved — and a provider that
//! does not read the field is sent nothing.

use super::*;
use crate::inference::testing::upstream::{Canned, ScriptedUpstream};

/// One finalized text turn that named `id` for itself.
fn stream(id: &str) -> String {
    format!(
        "event: message_start\ndata: {{\"message\":{{\"id\":\"{id}\",\"model\":\"m\",\
         \"usage\":{{\"input_tokens\":1,\"output_tokens\":1}}}}}}\n\n\
         event: content_block_start\ndata: {{\"index\":0,\"content_block\":\
         {{\"type\":\"text\",\"text\":\"\"}}}}\n\n\
         event: content_block_delta\ndata: {{\"index\":0,\"delta\":\
         {{\"type\":\"text_delta\",\"text\":\"ok\"}}}}\n\n\
         event: content_block_stop\ndata: {{\"index\":0}}\n\n\
         event: message_stop\ndata: {{}}\n\n"
    )
}

/// An upstream answering each request with the next of `streams`: what the second
/// request names depends on what the first response said.
fn serve(streams: Vec<String>) -> ScriptedUpstream {
    ScriptedUpstream::answering(streams.into_iter().map(Canned::stream))
}

/// The bodies `upstream` was sent, in arrival order.
fn bodies(upstream: &ScriptedUpstream) -> Vec<String> {
    upstream.requests().into_iter().map(|r| r.body).collect()
}

/// Two turns on a native Anthropic client: the first opts in with an explicit `null`, the
/// second names the first response's id. This is the whole mechanism — the object is the
/// opt-in that stores the fingerprint the next request is compared against
/// (docs/transport.md) — and it is also what keeps the chain off every other provider.
#[tokio::test]
async fn consecutive_turns_name_each_others_responses() {
    let upstream = serve(vec![stream("msg_a"), stream("msg_b")]);
    let client = Client::new(
        upstream.endpoint(Provider::Anthropic, "/v1/messages"),
        "m",
        "SYS",
    )
    .with_origin(crate::record::log::RequestOrigin::Turn);

    for _ in 0..2 {
        client
            .complete(&[], &[], ToolChoice::auto(), |_| {})
            .await
            .expect("the mock stream finalizes");
    }

    let [opening, second] = <[String; 2]>::try_from(bodies(&upstream)).unwrap();
    assert!(
        opening.contains(r#""diagnostics":{"previous_message_id":null}"#),
        "the first request opts in with nothing to compare: {opening}"
    );
    assert!(
        second.contains(r#""diagnostics":{"previous_message_id":"msg_a"}"#),
        "the second names the first response: {second}"
    );
}

/// The same two turns on a provider that does not read the field: the object is absent
/// from both bodies, byte for byte as if the feature did not exist. OpenRouter's Messages
/// skin accepts the object and answers a constant `null`, which a session cannot tell
/// apart from "no divergence" (docs/transport.md), so it is left off.
#[tokio::test]
async fn a_provider_without_the_field_is_sent_no_object() {
    let upstream = serve(vec![stream("msg_a"), stream("msg_b")]);
    let client = Client::new(
        upstream.endpoint(Provider::OpenRouter, "/v1/messages"),
        "m",
        "SYS",
    )
    .with_origin(crate::record::log::RequestOrigin::Turn);

    for _ in 0..2 {
        client
            .complete(&[], &[], ToolChoice::auto(), |_| {})
            .await
            .expect("the mock stream finalizes");
    }

    let bodies = bodies(&upstream);
    assert_eq!(bodies.len(), 2);
    for body in bodies {
        assert!(!body.contains("diagnostics"), "{body}");
    }
}

/// A triage rides the same client and the same prefix but sends its own prompt, so it must
/// not add itself to the chain: naming its response as the next turn's comparison base
/// would have the provider report a difference in `messages` that the loop never made
/// (docs/transport.md).
#[tokio::test]
async fn a_triage_neither_opts_in_nor_extends_the_chain() {
    let upstream = serve(vec![
        stream("msg_turn"),
        stream("msg_triage"),
        stream("msg_a"),
    ]);
    let client = Client::new(
        upstream.endpoint(Provider::Anthropic, "/v1/messages"),
        "m",
        "SYS",
    )
    .with_origin(crate::record::log::RequestOrigin::Turn);

    client
        .complete(&[], &[], ToolChoice::auto(), |_| {})
        .await
        .expect("the mock stream finalizes");
    // The triage, on the agent's own client, as the triager builds it.
    client
        .with_origin(crate::record::log::RequestOrigin::Triage)
        .complete(&[], &[], ToolChoice::auto(), |_| {})
        .await
        .expect("the mock stream finalizes");
    client
        .complete(&[], &[], ToolChoice::auto(), |_| {})
        .await
        .expect("the mock stream finalizes");

    let [_turn, triage, next] = <[String; 3]>::try_from(bodies(&upstream)).unwrap();
    assert!(
        !triage.contains("diagnostics"),
        "a triage sends its own prompt, so it neither opts in nor is compared: {triage}"
    );
    assert!(
        next.contains(r#""diagnostics":{"previous_message_id":"msg_turn"}"#),
        "the turn after the triage still names the last *turn*: {next}"
    );
}
