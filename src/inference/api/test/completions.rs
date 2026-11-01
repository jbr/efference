//! A described Completions connection end to end: `Client::complete` posts to
//! `{base}/chat/completions` with a bearer credential, and folds a recorded vendor
//! stream into a Completions turn (docs/transport.md).

use super::*;
use crate::inference::testing::upstream::{Canned, ScriptedUpstream};

#[tokio::test]
async fn a_described_completions_connection_round_trips() {
    let upstream = ScriptedUpstream::answering([Canned::stream(include_str!(
        "../completions/mistral_tool.sse"
    ))]);
    let endpoint = Endpoint::from_credentials(
        Provider::Custom(WireDialect::Completions),
        "KEY",
        None,
        Some(ScriptedUpstream::url("/v1")),
    )
    .unwrap()
    .with_http(upstream.http());
    let client = Client::new(endpoint, "mistral-small-latest", "PRE");
    let tools = [serde_json::json!({
        "name": "bash",
        "description": "run",
        "input_schema": {"type": "object"},
    })];
    let completion = client
        .complete(
            &[crate::record::project::WireTurn::text("list")],
            &tools,
            ToolChoice::auto(),
            |_| {},
        )
        .await
        .unwrap();
    let request = &upstream.requests()[0];

    assert_eq!(
        (request.method.as_str(), request.target.as_str()),
        ("POST", "/v1/chat/completions")
    );
    assert_eq!(request.header("authorization"), Some("Bearer KEY"));
    let body = request.json();
    assert_eq!(body["model"], "mistral-small-latest");
    assert_eq!(
        body["messages"][0],
        serde_json::json!({"role": "system", "content": "PRE"})
    );
    assert_eq!(
        body["messages"][1],
        serde_json::json!({"role": "user", "content": "list"})
    );
    assert_eq!(body["tools"][0]["function"]["name"], "bash");
    assert_eq!(body["stream_options"]["include_usage"], true);

    let calls = completion.assistant_output().tool_calls().unwrap();
    assert_eq!(calls.len(), 2);
    assert_eq!(
        completion.finish.as_ref().map(|f| &f.reason),
        Some(&FinishReason::ToolCalls)
    );
    assert!(completion.upstream_failure().is_none());
}

/// A connection with no credential sends no auth header at all, not an empty
/// `Bearer`.
#[tokio::test]
async fn a_connection_without_a_credential_sends_no_auth_header() {
    let upstream = ScriptedUpstream::answering([Canned::stream(include_str!(
        "../completions/mistral_text.sse"
    ))]);
    let endpoint = Endpoint::without_credential(
        Provider::Custom(WireDialect::Completions),
        ScriptedUpstream::url("/v1"),
    )
    .with_http(upstream.http());
    let client = Client::new(endpoint, "local", "PRE");
    client
        .complete(
            &[crate::record::project::WireTurn::text("hi")],
            &[],
            ToolChoice::auto(),
            |_| {},
        )
        .await
        .unwrap();
    let request = &upstream.requests()[0];
    assert_eq!(
        request.header("authorization"),
        None,
        "{:?}",
        request.headers
    );
}
