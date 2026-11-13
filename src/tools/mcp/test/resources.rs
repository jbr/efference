//! `resources__<server>` against a scripted server: mounted only for a server that offers
//! resources, a listing without `uri`, a read with one, and every failure as a paired result.

use super::*;

/// A server offering resources: two pages of resources, one template, and reads that cover
/// each shape an answer can take.
fn resource_server() -> Box<Script> {
    resource_server_with(|cursor| match cursor {
        None => json!({
            "resources": [{
                "uri": "fake://models", "name": "models", "title": "Available models",
                "mimeType": "text/plain", "description": "Every configured model."
            }],
            "nextCursor": "page-2"
        }),
        Some("page-2") => json!({ "resources": [{ "uri": "fake://second", "name": "second" }] }),
        Some(other) => panic!("unexpected cursor {other}"),
    })
}

fn resource_server_with(list: fn(Option<&str>) -> Value) -> Box<Script> {
    Box::new(move |request: &JsonRpcRequest| {
        let params = request.params.clone().unwrap_or(Value::Null);
        let response = match request.method.as_str() {
            "server/discover" => return declines_discovery(request),
            "initialize" => JsonRpcResponse::success(
                request.id.clone(),
                json!({
                    "protocolVersion": "2025-06-18",
                    "capabilities": { "tools": {}, "resources": {} },
                    "serverInfo": { "name": "fake", "version": "0" }
                }),
            ),
            "tools/list" => {
                JsonRpcResponse::success(request.id.clone(), json!({ "tools": [tool("zeta")] }))
            }
            "resources/list" => JsonRpcResponse::success(
                request.id.clone(),
                list(params.get("cursor").and_then(Value::as_str)),
            ),
            "resources/templates/list" => JsonRpcResponse::success(
                request.id.clone(),
                json!({ "resourceTemplates": [{
                    "uriTemplate": "fake://models{?q}", "name": "models-query",
                    "description": "Search the models."
                }]}),
            ),
            "resources/read" => {
                let uri = params["uri"].as_str().unwrap();
                match uri {
                    "fake://models" => JsonRpcResponse::success(
                        request.id.clone(),
                        json!({ "contents": [
                            { "uri": uri, "mimeType": "text/plain", "text": "alpha\nbeta" },
                            { "uri": "fake://logo", "mimeType": "image/png", "blob": "AA==" }
                        ]}),
                    ),
                    "fake://empty" => {
                        JsonRpcResponse::success(request.id.clone(), json!({ "contents": [] }))
                    }
                    "fake://interim" => JsonRpcResponse::success(
                        request.id.clone(),
                        json!({ "resultType": "input_required", "requestState": "later" }),
                    ),
                    "fake://legacy-missing" => JsonRpcResponse::error(
                        request.id.clone(),
                        JsonRpcError {
                            code: -32002,
                            message: "Resource not found".into(),
                            data: None,
                        },
                    ),
                    "fake://broken" => JsonRpcResponse::error(
                        request.id.clone(),
                        JsonRpcError::internal("backend down"),
                    ),
                    other => JsonRpcResponse::error(
                        request.id.clone(),
                        JsonRpcError::resource_not_found(other),
                    ),
                }
            }
            other => {
                JsonRpcResponse::error(request.id.clone(), JsonRpcError::method_not_found(other))
            }
        };
        vec![JsonRpcMessage::Response(response)]
    })
}

async fn mounted() -> ConnectedProvider {
    let (transport, _) = loopback(resource_server());
    ConnectedProvider::mount_transport("fake", transport)
        .await
        .unwrap()
}

async fn resources(provider: &ConnectedProvider, input: Value) -> RawResult {
    run(provider, &call("resources__fake", input)).await
}

#[tokio::test]
async fn a_server_offering_resources_gets_a_resource_tool_in_efferences_namespace() {
    let provider = mounted().await;
    assert!(provider.handles("resources__fake"));
    assert!(!provider.handles("fake__resources"));

    // Last, after the server's own sorted tools, so the table stays byte-stable.
    let definitions = provider.definitions();
    let names: Vec<&str> = definitions
        .iter()
        .map(|d| d["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["fake__zeta", "resources__fake"]);
    let schema = &definitions[1]["input_schema"];
    assert_eq!(schema["properties"]["uri"]["type"], "string");
    assert!(schema.get("required").is_none());

    // The spotter's brief names the server the reads go to.
    let brief = provider.brief("resources__fake").unwrap();
    assert_eq!(brief.server, Some("fake"));
    let hints = brief.definition.annotations.as_ref().unwrap();
    assert_eq!(hints.read_only_hint, Some(true));
}

#[tokio::test]
async fn a_server_without_resources_gets_no_resource_tool() {
    let (transport, _) = loopback(scripted_server());
    let provider = ConnectedProvider::mount_transport("fake", transport)
        .await
        .unwrap();
    assert!(!provider.handles("resources__fake"));
}

#[tokio::test]
async fn the_resource_tool_is_enabled_apart_from_the_servers_tools() {
    let mut provider = mounted().await;
    assert!(provider.retain_enabled(&|name: &str| name != "resources__fake"));
    assert!(provider.handles("fake__zeta"));
    assert!(!provider.handles("resources__fake"));
}

#[tokio::test]
async fn without_a_uri_the_tool_lists_every_page_of_resources_and_templates() {
    let provider = mounted().await;
    for input in [json!({}), json!({ "uri": null }), json!({ "uri": "  " })] {
        let result = resources(&provider, input).await;
        assert!(!result.is_error, "{}", result.content);
        assert_eq!(
            result.content,
            "Resources from `fake`:\n\
             - fake://models — Available models (text/plain)\n  Every configured model.\n\
             - fake://second — second\n\n\
             URI templates from `fake` — fill in the {…} variables, then read the URI:\n\
             - fake://models{?q} — models-query\n  Search the models."
        );
    }
}

#[tokio::test]
async fn a_read_renders_its_contents_as_embedded_resources_are() {
    let provider = mounted().await;
    let result = resources(&provider, json!({ "uri": "fake://models" })).await;
    assert!(!result.is_error, "{}", result.content);
    assert_eq!(
        result.content,
        "fake://models:\nalpha\nbeta\n\n[binary resource fake://logo (image/png) — not rendered]"
    );
    let empty = resources(&provider, json!({ "uri": "fake://empty" })).await;
    assert!(!empty.is_error);
    assert_eq!(empty.content, "fake://empty is empty");
}

#[tokio::test]
async fn a_missing_resource_points_back_at_the_listing_on_either_revisions_code() {
    let provider = mounted().await;
    for uri in ["fake://nope", "fake://legacy-missing"] {
        let result = resources(&provider, json!({ "uri": uri })).await;
        assert!(result.is_error);
        assert!(
            result
                .content
                .starts_with(&format!("`fake` has no resource at {uri}"))
                && result
                    .content
                    .ends_with("Call `resources__fake` without `uri` to list what it serves."),
            "{}",
            result.content
        );
    }
}

#[tokio::test]
async fn every_other_failure_is_a_paired_error_result() {
    let provider = mounted().await;
    let broken = resources(&provider, json!({ "uri": "fake://broken" })).await;
    assert!(broken.is_error);
    assert!(
        broken.content.contains("backend down"),
        "{}",
        broken.content
    );

    let interim = resources(&provider, json!({ "uri": "fake://interim" })).await;
    assert!(interim.is_error);
    assert!(
        interim.content.contains("the server asked to be retried"),
        "{}",
        interim.content
    );

    let malformed = resources(&provider, json!({ "uri": 7 })).await;
    assert!(malformed.is_error);
    assert_eq!(malformed.content, "`uri` must be a string, not 7");
}

#[tokio::test]
async fn a_repeated_cursor_ends_the_listing_instead_of_looping() {
    let (transport, _) = loopback(resource_server_with(
        |_| json!({ "resources": [], "nextCursor": "again" }),
    ));
    let provider = ConnectedProvider::mount_transport("fake", transport)
        .await
        .unwrap();
    let result = resources(&provider, json!({})).await;
    assert!(result.is_error);
    assert_eq!(
        result.content,
        "`fake` repeated the resources/list cursor \"again\"; the listing would never end"
    );
}
