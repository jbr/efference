use super::*;
use crate::inference::api::ListedModel;
use crate::record::log::{Provider, ToolCall};
use crate::tools::Toolbox;

#[tokio::test]
async fn a_host_catalog_is_readable_without_any_agent_tools() {
    let roots = Roots::new();
    let cwd = roots.0.join("one");
    let host = Host::new(one_connection_registry(), baseline_env());
    host.catalog
        .refresh_with(
            "test",
            Provider::Anthropic,
            crate::inference::catalog::CATALOG_TTL,
            || async { Ok(vec![ListedModel::new("native-v2").named("Friendly Model")]) },
        )
        .await;
    let mut toolbox = Toolbox::new(Vec::new());
    toolbox.mount_resources(host.model_resource_reader(&cwd));
    toolbox.retain_enabled(|_| false);
    let call = ToolCall::new(
        "read",
        "models-read",
        serde_json::json!({"paths": ["efference://guide/supervising", "efference://models?connection=test&q=friendly"]}),
    );
    let result = toolbox.read_resources(&call).await.unwrap();
    assert!(!result.is_error, "{}", result.content);
    assert!(
        result
            .content
            .contains("- test/native-v2 — Friendly Model; price=unknown")
    );
    assert!(result.content.contains("1 matches; showing 1"));
    assert!(result.content.contains("\n\n---\n\nModel catalog"));
    drop(host);
    let result = toolbox.read_resources(&call).await.unwrap();
    assert!(result.is_error);
    assert!(result.content.contains("host has shut down"));
}

#[tokio::test]
async fn mcp_model_resource_reads_through_the_host_http_catalog() {
    use crate::clients::control_tools::{
        ClientExecutor, Control, Executor, mcp, resources::ModelResources,
    };
    use mcplease::types::{JsonRpcRequest, RequestContext};
    let roots = Roots::new();
    let history = crate::config::history::History::at(roots.0.join("history.db"));
    history.record(&crate::config::history::Pick {
        at: crate::config::history::now(),
        cwd: roots.0.join("two").display().to_string(),
        config_key: crate::config::history::BUILTIN_CONFIG_KEY.into(),
        connection: "test".into(),
        provider: "anthropic".into(),
        model: "native-v2".into(),
    });
    let host = Host::new(
        one_connection_registry().with_history(history),
        baseline_env(),
    );
    host.catalog
        .refresh_with(
            "test",
            Provider::Anthropic,
            crate::inference::catalog::CATALOG_TTL,
            || async {
                Ok(vec![
                    ListedModel::new("native-v2").named("Friendly Model"),
                    ListedModel::new("aaa").named("Friendly Model"),
                ])
            },
        )
        .await;
    let client = host_client(&host, "unused").await;
    let state = Control::new(Executor::Client(ClientExecutor::over(client)), false);
    let config = mcplease::ServerConfig::new(mcplease::server_info!());
    let uri = "efference://models?connection=test&q=Friendly+Model&limit=1&offset=0";
    let request: JsonRpcRequest = serde_json::from_value(serde_json::json!({
        "jsonrpc":"2.0", "id":1, "method":"resources/read", "params":{"uri":uri}
    }))
    .unwrap();
    let context = RequestContext::from_request(&request);
    let response = mcplease::handle_request_with_resources::<mcp::Tools, ModelResources, _>(
        request,
        state.clone(),
        context,
        &config,
    )
    .await
    .unwrap();
    assert!(response.error.is_none(), "{:?}", response.error);
    let result = response.result.unwrap();
    assert_eq!(result["contents"][0]["uri"], uri);
    assert_eq!(result["contents"][0]["mimeType"], "text/plain");
    let text = result["contents"][0]["text"].as_str().unwrap();
    assert!(
        text.contains("test/native-v2 — Friendly Model; recent #1; price=unknown"),
        "{text}"
    );
    assert!(text.contains("2 matches; showing 1"), "{text}");
    assert!(!text.contains("test/aaa"));
    let uri = "efference://models?connection=nope";
    let request: JsonRpcRequest = serde_json::from_value(serde_json::json!({
        "jsonrpc":"2.0", "id":2, "method":"resources/read", "params":{"uri":uri}
    }))
    .unwrap();
    let context = RequestContext::from_request(&request);
    let response = mcplease::handle_request_with_resources::<mcp::Tools, ModelResources, _>(
        request, state, context, &config,
    )
    .await
    .unwrap();
    assert!(response.error.is_none(), "{:?}", response.error);
    let result = response.result.unwrap();
    assert_eq!(result["contents"][0]["uri"], uri);
    let text = result["contents"][0]["text"].as_str().unwrap();
    assert!(text.contains("no connection named \"nope\""), "{text}");
    assert!(text.contains("Connections: test"), "{text}");
    assert!(text.contains("Filters combine"), "{text}");
}
