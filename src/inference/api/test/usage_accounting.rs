use super::*;
use crate::inference::testing::upstream::{Canned, ScriptedUpstream};

#[tokio::test]
async fn low_reported_usage_does_not_reject_a_successful_response() {
    let stream = concat!(
        "event: message_start\ndata: {\"message\":{\"id\":\"msg_ok\",\"model\":\"m\",",
        "\"usage\":{\"input_tokens\":10560,\"output_tokens\":1}}}\n\n",
        "event: content_block_start\ndata: {\"index\":0,\"content_block\":",
        "{\"type\":\"text\",\"text\":\"\"}}\n\n",
        "event: content_block_delta\ndata: {\"index\":0,\"delta\":",
        "{\"type\":\"text_delta\",\"text\":\"ok\"}}\n\n",
        "event: content_block_stop\ndata: {\"index\":0}\n\n",
        "event: message_stop\ndata: {}\n\n",
    );
    let upstream = ScriptedUpstream::answering([Canned::stream(stream)]);
    let client = Client::new(
        upstream.endpoint(Provider::OpenRouter, "/v1/messages"),
        "m",
        "system prompt ".repeat(4_000),
    );
    let tools = [serde_json::json!({
        "name": "receipt",
        "description": "tool description ".repeat(6_000),
        "input_schema": {"type": "object", "properties": {}}
    })];

    let completion = client
        .complete(&[], &tools, ToolChoice::auto(), |_| {})
        .await
        .expect("the stream finalizes despite the low reported count");

    assert_eq!(completion.usage.prompt_tokens(), 10_560);
    assert!(completion.terminal_failure().is_none());
    assert!(completion.upstream_failure().is_none());
    assert_eq!(completion.assistant_output().text(), ["ok"]);
}
