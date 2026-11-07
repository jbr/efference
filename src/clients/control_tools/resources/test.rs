use super::super::{Fallback, mcp};
use super::*;
use serde_json::{Value, json};

async fn request(method: &str, params: Value) -> JsonRpcResponse {
    let request: JsonRpcRequest = serde_json::from_value(json!({
        "jsonrpc": "2.0", "id": 1, "method": method, "params": params,
    }))
    .unwrap();
    let state = Control::new(
        Executor::client("/nonexistent/host.sock".into(), Fallback::None),
        false,
    );
    let config = mcplease::ServerConfig::new(mcplease::server_info!())
        .with_instructions(super::super::MCP_INSTRUCTIONS);
    let context = RequestContext::from_request(&request);
    mcplease::handle_request_with_resources::<mcp::Tools, ModelResources, _>(
        request, state, context, &config,
    )
    .await
    .unwrap()
}

#[tokio::test]
async fn model_resources_are_discoverable_without_a_running_host() {
    let response = request("initialize", json!({"protocolVersion":"2025-11-25", "capabilities":{}, "clientInfo":{"name":"test", "version":"1"}})).await;
    assert!(response.error.is_none(), "{:?}", response.error);
    let result = response.result.unwrap();
    assert!(result["capabilities"]["resources"].is_object());
    assert!(result["capabilities"]["tools"].is_object());
    assert!(result["instructions"].as_str().unwrap().contains(URI));

    let result = request("resources/list", json!({})).await.result.unwrap();
    assert_eq!(result["resources"].as_array().unwrap().len(), 1);
    assert_eq!(result["resources"][0]["uri"], URI);
    assert_eq!(result["resources"][0]["mimeType"], "text/plain");
    assert!(result["nextCursor"].is_null());

    let result = request("resources/templates/list", json!({}))
        .await
        .result
        .unwrap();
    assert_eq!(
        result["resourceTemplates"][0]["uriTemplate"],
        format!("{URI}{{?connection,q,role,limit,offset}}")
    );
}

#[tokio::test]
async fn model_resource_errors_are_protocol_errors() {
    for (method, params) in [
        ("resources/list", json!({"cursor":"not-issued"})),
        ("resources/templates/list", json!({"cursor":"not-issued"})),
        ("resources/read", json!({"uri":"file:///secret"})),
    ] {
        let response = request(method, params).await;
        assert!(response.result.is_none());
        assert_eq!(response.error.unwrap().code, -32602);
    }
    let response = request("resources/read", json!({"uri":URI})).await;
    assert!(response.result.is_none());
    assert!(response.error.unwrap().message.contains("no host"));
}

#[tokio::test]
async fn mistaken_model_queries_are_readable_contents_without_a_host() {
    for suffix in [
        "limit=0",
        "role=bogus",
        "wat=x",
        "q=%GG",
        "q=a&q=b",
        "connection=nope&role=bogus&limit=0",
    ] {
        let uri = format!("{URI}?{suffix}");
        let response = request("resources/read", json!({"uri":uri})).await;
        assert!(response.error.is_none(), "{:?}", response.error);
        let result = response.result.unwrap();
        assert_eq!(result["contents"][0]["uri"], uri);
        let text = result["contents"][0]["text"].as_str().unwrap();
        assert!(text.starts_with("Invalid model query:"), "{text}");
        assert!(text.contains("Filters combine"), "{text}");
    }
}
