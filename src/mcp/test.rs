use std::sync::{Arc, Mutex};

use mcplease::types::{
    CallToolResult, JsonRpcError, JsonRpcMessage, JsonRpcRequest, JsonRpcResponse, RequestId,
    Tool as ToolDefinition,
};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, DuplexStream, ReadHalf, WriteHalf};

use super::stdio::JsonRpcPipe;
use super::*;
use crate::log::{CallId, ToolCall};

/// The client half of an in-memory duplex — the framing and mount logic
/// without a subprocess (`Transport::Loopback`).
pub(super) type LoopbackPipe =
    JsonRpcPipe<tokio::io::BufReader<ReadHalf<DuplexStream>>, WriteHalf<DuplexStream>>;

/// Everything a scripted server sends in reaction to one client request.
type Script = dyn FnMut(&JsonRpcRequest) -> Vec<JsonRpcMessage> + Send;

/// Spawn a scripted server on the other end of a duplex and hand back the
/// connected loopback transport. The script sees each client *request*;
/// client notifications and responses (to server-initiated requests) are
/// recorded in the returned inbox instead.
fn loopback(mut script: Box<Script>) -> (Transport, Arc<Mutex<Vec<JsonRpcMessage>>>) {
    let (client_end, server_end) = tokio::io::duplex(64 * 1024);
    let inbox = Arc::new(Mutex::new(Vec::new()));
    let seen = Arc::clone(&inbox);
    tokio::spawn(async move {
        let (read, mut write) = tokio::io::split(server_end);
        let mut lines = BufReader::new(read).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            let Ok(message) = serde_json::from_str::<JsonRpcMessage>(&line) else {
                continue;
            };
            match message {
                JsonRpcMessage::Request(request) => {
                    for outgoing in script(&request) {
                        let mut line = serde_json::to_string(&outgoing).unwrap();
                        line.push('\n');
                        write.write_all(line.as_bytes()).await.unwrap();
                    }
                }
                other => seen.lock().unwrap().push(other),
            }
        }
    });
    let (read, write) = tokio::io::split(client_end);
    let pipe = JsonRpcPipe::new(tokio::io::BufReader::new(read), write);
    (Transport::Loopback(pipe), inbox)
}

/// How a server on any revision before `2026-07-28` answers the
/// `server/discover` probe — which is exactly how the client learns to fall
/// back to the handshake. Every script below opens with it.
fn declines_discovery(request: &JsonRpcRequest) -> Vec<JsonRpcMessage> {
    vec![JsonRpcMessage::Response(JsonRpcResponse::error(
        request.id.clone(),
        JsonRpcError::method_not_found("server/discover"),
    ))]
}

fn initialize_result(version: &str) -> Value {
    json!({
        "protocolVersion": version,
        "capabilities": { "tools": {} },
        "serverInfo": { "name": "fake", "version": "0" }
    })
}

fn tool(name: &str) -> ToolDefinition {
    let mut tool = ToolDefinition::new(
        name,
        json!({ "type": "object", "properties": { "x": { "type": "string" } } }),
    );
    tool.description = Some(format!("tool {name}"));
    tool
}

/// A well-behaved server: handshake, a paginated two-page tool list advertised
/// in *unsorted* order, and echoing tool calls.
fn scripted_server() -> Box<Script> {
    Box::new(|request: &JsonRpcRequest| {
        let response = match request.method.as_str() {
            "initialize" => {
                JsonRpcResponse::success(request.id.clone(), initialize_result("2025-06-18"))
            }
            "tools/list" => {
                let cursor = request
                    .params
                    .as_ref()
                    .and_then(|p| p.get("cursor"))
                    .and_then(Value::as_str);
                let page = match cursor {
                    None => json!({
                        "tools": [tool("zeta")],
                        "nextCursor": "page-2"
                    }),
                    Some("page-2") => {
                        json!({ "tools": [tool("alpha")] })
                    }
                    Some(other) => panic!("unexpected cursor {other}"),
                };
                JsonRpcResponse::success(request.id.clone(), page)
            }
            "tools/call" => {
                let params = request.params.as_ref().unwrap();
                let name = params["name"].as_str().unwrap();
                match name {
                    "zeta" => JsonRpcResponse::success(
                        request.id.clone(),
                        serde_json::to_value(CallToolResult::text(format!(
                            "zeta got {}",
                            params["arguments"]["x"].as_str().unwrap_or("nothing")
                        )))
                        .unwrap(),
                    ),
                    "alpha" => JsonRpcResponse::success(
                        request.id.clone(),
                        serde_json::to_value(CallToolResult::error("alpha broke")).unwrap(),
                    ),
                    other => JsonRpcResponse::error(
                        request.id.clone(),
                        JsonRpcError::invalid_params(format!("unknown tool {other}")),
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

fn call(name: &str, input: Value) -> ToolCall {
    ToolCall {
        id: CallId("toolu_1".into()),
        name: name.to_string(),
        input,
    }
}

async fn run(provider: &ConnectedProvider, tool_call: &ToolCall) -> crate::log::RawResult {
    match provider.run(tool_call, std::future::pending()).await {
        Outcome::Done(result) => result,
        Outcome::Detached(_) => panic!("a connected call never detaches"),
    }
}

#[tokio::test]
async fn mount_discovers_paginates_namespaces_and_sorts() {
    let (transport, _) = loopback(scripted_server());
    let provider = ConnectedProvider::mount_transport("fake", transport)
        .await
        .unwrap();

    // Both pages arrived; names are namespaced and sorted by tool name even
    // though the server advertised zeta first.
    assert!(provider.handles("fake__alpha"));
    assert!(provider.handles("fake__zeta"));
    assert!(!provider.handles("alpha"));

    let definitions = provider.definitions();
    let names: Vec<&str> = definitions
        .iter()
        .map(|d| d["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["fake__alpha", "fake__zeta"]);
    // The Anthropic shape: snake_case input_schema, carried verbatim.
    assert_eq!(
        definitions[0]["input_schema"]["properties"]["x"]["type"],
        json!("string")
    );
}

#[tokio::test]
async fn mount_confirms_the_handshake_with_initialized() {
    let (transport, inbox) = loopback(scripted_server());
    ConnectedProvider::mount_transport("fake", transport)
        .await
        .unwrap();
    let notified = inbox.lock().unwrap().iter().any(|message| {
        matches!(
            message,
            JsonRpcMessage::Notification(n) if n.method == "notifications/initialized"
        )
    });
    assert!(notified);
}

#[tokio::test]
async fn a_call_lands_text_and_an_is_error_result_stays_a_paired_result() {
    let (transport, _) = loopback(scripted_server());
    let provider = ConnectedProvider::mount_transport("fake", transport)
        .await
        .unwrap();

    let ok = run(&provider, &call("fake__zeta", json!({ "x": "hi" }))).await;
    assert_eq!(ok.content, "zeta got hi");
    assert!(!ok.is_error);

    let failed = run(&provider, &call("fake__alpha", json!({}))).await;
    assert_eq!(failed.content, "alpha broke");
    assert!(failed.is_error);
}

#[tokio::test]
async fn a_protocol_error_becomes_an_error_result_not_a_panic() {
    let (transport, _) = loopback(scripted_server());
    let provider = ConnectedProvider::mount_transport("fake", transport)
        .await
        .unwrap();
    // The provider only ever dispatches names it handles, but a hostile or
    // confused wire name must still land as a paired error result.
    let result = run(&provider, &call("fake__missing", json!({}))).await;
    assert!(result.is_error);
    assert!(result.content.contains("unknown tool"));
}

#[tokio::test]
async fn interleaved_notifications_requests_and_stale_responses_are_stepped_over() {
    let script: Box<Script> = Box::new(|request: &JsonRpcRequest| match request.method.as_str() {
        "server/discover" => declines_discovery(request),
        "initialize" => vec![
            // A notification the client should skip…
            JsonRpcMessage::Notification(mcplease::types::JsonRpcNotification::new(
                "notifications/message",
                Some(json!({ "level": "info", "data": "starting up" })),
            )),
            // …a server-initiated ping it should answer…
            JsonRpcMessage::Request(JsonRpcRequest::new(
                RequestId::String("server-ping".into()),
                "ping",
                None,
            )),
            // …a stale response to nobody…
            JsonRpcMessage::Response(JsonRpcResponse::success(
                RequestId::Integer(9999),
                json!({}),
            )),
            // …then the real answer.
            JsonRpcMessage::Response(JsonRpcResponse::success(
                request.id.clone(),
                initialize_result("2025-06-18"),
            )),
        ],
        "tools/list" => vec![JsonRpcMessage::Response(JsonRpcResponse::success(
            request.id.clone(),
            json!({ "tools": [] }),
        ))],
        other => panic!("unexpected method {other}"),
    });
    let (transport, inbox) = loopback(script);
    ConnectedProvider::mount_transport("fake", transport)
        .await
        .unwrap();

    // The server's ping got a success response back.
    let ping_answered = inbox.lock().unwrap().iter().any(|message| {
        matches!(
            message,
            JsonRpcMessage::Response(r)
                if r.id == Some(RequestId::String("server-ping".into())) && r.result.is_some()
        )
    });
    assert!(ping_answered);
}

#[tokio::test]
async fn an_unsupported_negotiated_version_fails_the_mount() {
    let script: Box<Script> = Box::new(|request: &JsonRpcRequest| {
        if request.method == "server/discover" {
            return declines_discovery(request);
        }
        vec![JsonRpcMessage::Response(JsonRpcResponse::success(
            request.id.clone(),
            initialize_result("1999-12-31"),
        ))]
    });
    let (transport, _) = loopback(script);
    let Err(error) = ConnectedProvider::mount_transport("fake", transport).await else {
        panic!("expected the mount to fail");
    };
    assert!(error.to_string().contains("1999-12-31"));
}

#[tokio::test]
async fn a_dead_server_fails_the_call_as_an_error_result() {
    // Handshake and discovery succeed; the first tool call panics the server
    // task, dropping its end of the pipe — the client sees EOF mid-wait and
    // must land it as a paired error result, never a hang or a panic.
    let script: Box<Script> = Box::new(|request: &JsonRpcRequest| match request.method.as_str() {
        "server/discover" => declines_discovery(request),
        "initialize" => vec![JsonRpcMessage::Response(JsonRpcResponse::success(
            request.id.clone(),
            initialize_result("2025-06-18"),
        ))],
        "tools/list" => vec![JsonRpcMessage::Response(JsonRpcResponse::success(
            request.id.clone(),
            json!({ "tools": [tool("zeta")] }),
        ))],
        _ => panic!("server died"),
    });
    let (transport, _) = loopback(script);
    let provider = ConnectedProvider::mount_transport("fake", transport)
        .await
        .unwrap();
    let result = run(&provider, &call("fake__zeta", json!({ "x": "hi" }))).await;
    assert!(result.is_error);
    assert!(result.content.contains("closed"), "{}", result.content);
}

#[tokio::test]
async fn a_stateless_server_is_mounted_without_a_handshake() {
    // A `2026-07-28` server: it answers `server/discover` and has no
    // `initialize` at all, so the mount must settle on the probe alone and
    // send no `notifications/initialized`.
    let script: Box<Script> = Box::new(|request: &JsonRpcRequest| match request.method.as_str() {
        "server/discover" => vec![JsonRpcMessage::Response(JsonRpcResponse::success(
            request.id.clone(),
            json!({
                "supportedVersions": ["2026-07-28"],
                "capabilities": { "tools": {} },
                "resultType": "complete",
            }),
        ))],
        "tools/list" => vec![JsonRpcMessage::Response(JsonRpcResponse::success(
            request.id.clone(),
            json!({ "tools": [tool("zeta")] }),
        ))],
        "tools/call" => {
            // The stateless revision carries the client's identity in every
            // request's `_meta` instead of a handshake; a server on it is
            // entitled to read one.
            let context = mcplease::types::RequestContext::from_params(request.params.as_ref());
            let caller = context.client_info.expect("clientInfo in _meta");
            vec![JsonRpcMessage::Response(JsonRpcResponse::success(
                request.id.clone(),
                serde_json::to_value(CallToolResult::text(format!("called by {}", caller.name)))
                    .unwrap(),
            ))]
        }
        other => panic!("a stateless server has no {other}"),
    });
    let (transport, inbox) = loopback(script);
    let provider = ConnectedProvider::mount_transport("fake", transport)
        .await
        .unwrap();

    let result = run(&provider, &call("fake__zeta", json!({ "x": "hi" }))).await;
    assert_eq!(result.content, "called by efference");
    assert!(!result.is_error);

    // Nothing to confirm: there is no session.
    let confirmed = inbox.lock().unwrap().iter().any(|message| {
        matches!(message, JsonRpcMessage::Notification(n) if n.method == "notifications/initialized")
    });
    assert!(!confirmed);
}

#[tokio::test]
async fn an_interim_result_lands_as_an_error_naming_what_was_asked_for() {
    // A 2026-07-28 server answering `tools/call` with `resultType:
    // "input_required"`. Parsed as a `CallToolResult` this would look like a
    // successful call with empty content; it must instead pair the `tool_use`
    // with an error the model can act on.
    let script: Box<Script> = Box::new(|request: &JsonRpcRequest| match request.method.as_str() {
        "server/discover" => declines_discovery(request),
        "initialize" => vec![JsonRpcMessage::Response(JsonRpcResponse::success(
            request.id.clone(),
            initialize_result("2025-06-18"),
        ))],
        "tools/list" => vec![JsonRpcMessage::Response(JsonRpcResponse::success(
            request.id.clone(),
            json!({ "tools": [tool("zeta")] }),
        ))],
        _ => vec![JsonRpcMessage::Response(JsonRpcResponse::success(
            request.id.clone(),
            json!({
                "resultType": "input_required",
                "inputRequests": {
                    "ask-1": { "method": "elicitation/create", "params": {} }
                }
            }),
        ))],
    });
    let (transport, _) = loopback(script);
    let provider = ConnectedProvider::mount_transport("fake", transport)
        .await
        .unwrap();
    let result = run(&provider, &call("fake__zeta", json!({ "x": "hi" }))).await;
    assert!(result.is_error);
    assert!(
        result.content.contains("elicitation/create"),
        "{}",
        result.content
    );
}

/// Live end-to-end over streamable HTTP against a real, unauthenticated
/// server. Network + third-party dependency, so ignored by default:
/// `cargo test -- --ignored live_ferritin`.
#[tokio::test]
#[ignore = "network: mounts https://ferritin.rs/mcp"]
async fn live_ferritin_mount_and_call() {
    let provider = ConnectedProvider::mount(
        "ferritin",
        &crate::config::McpTransport::Http {
            url: "https://ferritin.rs/mcp".into(),
        },
        std::path::Path::new("."),
        None,
    )
    .await
    .unwrap();

    let definitions = provider.definitions();
    assert!(!definitions.is_empty());
    let names: Vec<&str> = definitions
        .iter()
        .map(|d| d["name"].as_str().unwrap())
        .collect();
    eprintln!("ferritin advertises: {names:?}");
    assert!(names.iter().all(|n| n.starts_with("ferritin__")));

    // Call whichever tool is first with plausible search arguments; a real
    // result or a well-formed is_error both prove the dispatch path.
    let result = run(
        &provider,
        &call(
            "ferritin__search",
            json!({ "query": "Deserialize", "crate": "serde" }),
        ),
    )
    .await;
    eprintln!(
        "ferritin__search → is_error={} content:\n{}",
        result.is_error,
        &result.content[..result.content.len().min(600)]
    );
    assert!(!result.is_error, "{}", result.content);
}

/// Live end-to-end over stdio against the locally installed cargo-mcp — a
/// real server on an *old* mcplease (0.2.2, protocol 2024-11-05), so this
/// also proves the version negotiation downgrade. `HOME` is overridden to a
/// scratch dir because cargo-mcp persists its session store under
/// `~/.ai-tools` (shared across AI tools) — the test must not touch the real
/// one; the override also exercises the config's `env` passthrough.
/// `cargo test -- --ignored live_cargo_mcp`.
#[tokio::test]
#[ignore = "spawns the locally installed cargo-mcp binary"]
async fn live_cargo_mcp_stdio_mount_and_call() {
    let home = std::env::temp_dir().join(format!("efference-mcp-smoke-{}", std::process::id()));
    std::fs::create_dir_all(&home).unwrap();
    let provider = ConnectedProvider::mount(
        "cargo",
        &crate::config::McpTransport::Stdio {
            command: "cargo-mcp".into(),
            args: vec!["serve".into()],
            env: [("HOME".to_string(), home.display().to_string())].into(),
        },
        std::path::Path::new("."),
        None,
    )
    .await
    .unwrap();

    let definitions = provider.definitions();
    let names: Vec<&str> = definitions
        .iter()
        .map(|d| d["name"].as_str().unwrap())
        .collect();
    eprintln!("cargo-mcp advertises: {names:?}");
    assert!(provider.handles("cargo__cargo_check"));
    assert!(provider.handles("cargo__set_working_directory"));

    // A real call with a side effect confined to the scratch HOME.
    let result = run(
        &provider,
        &call("cargo__set_working_directory", json!({ "path": "." })),
    )
    .await;
    eprintln!(
        "cargo__set_working_directory → is_error={} content: {}",
        result.is_error, result.content
    );
    assert!(!result.is_error, "{}", result.content);

    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn multimodal_blocks_render_as_placeholders_and_structured_content_as_json() {
    let result = CallToolResult {
        content: vec![
            mcplease::types::ContentBlock::text("plain"),
            mcplease::types::ContentBlock::Image {
                data: "aGVsbG8=".into(),
                mime_type: "image/png".into(),
                annotations: None,
                meta: None,
            },
        ],
        ..CallToolResult::default()
    };
    let rendered = render_result(result);
    assert!(rendered.content.starts_with("plain\n\n[image image/png"));
    assert!(!rendered.is_error);

    let structured_only = CallToolResult {
        structured_content: Some(json!({ "answer": 42 })),
        ..CallToolResult::default()
    };
    let rendered = render_result(structured_only);
    assert!(rendered.content.contains("\"answer\": 42"));
}

#[test]
fn display_name_dots_the_namespace_boundary_only() {
    // The common case: what the operator reads instead of `ferritin__get`.
    assert_eq!(display_name("ferritin__get"), "ferritin.get");
    // An embedded tool name has no boundary to find, so it passes through
    // unchanged — the same invariant that makes namespacing collision-free.
    assert_eq!(display_name("edit"), "edit");
    assert_eq!(display_name("undo_edit"), "undo_edit");
    // Only the *first* `__` is the boundary. A server whose own tool name
    // contains `__` keeps it: `get__raw` is that server's name for the tool,
    // not a second namespace.
    assert_eq!(display_name("ferritin__get__raw"), "ferritin.get__raw");
}

/// Nothing missing means no section: a heading announcing that everything
/// mounted is noise, and it would sit in the cached prefix forever.
#[test]
fn nothing_unmounted_renders_no_section() {
    assert_eq!(unmounted_section(&[]), None);
}

/// The section exists to stop the model from reading its tool table as the
/// whole truth — so it must name the server, give the reason, and say what to
/// do when asked for the missing capability.
#[test]
fn an_unmounted_section_names_each_server_and_why() {
    let section = unmounted_section(&[
        Unmounted {
            name: "sentry".into(),
            reason: "server requires authorization (401)".into(),
        },
        Unmounted {
            name: "github".into(),
            reason: "could not spawn `gh-mcp`: No such file or directory".into(),
        },
    ])
    .unwrap();

    assert!(section.contains("`sentry`"), "{section}");
    assert!(
        section.contains("server requires authorization (401)"),
        "{section}"
    );
    assert!(section.contains("`github`"), "{section}");
    assert!(section.contains("No such file or directory"), "{section}");
    // The instruction that converts a wrong answer into a useful one.
    assert!(section.contains("configured but"), "{section}");
    assert!(section.contains("do not answer as though"), "{section}");
}

mod standard_headers {
    use super::super::http::{encode_header_value, mcp_name, standard_headers};
    use mcplease::types::{JsonRpcMessage, JsonRpcNotification, JsonRpcRequest};

    fn request(method: &str, params: serde_json::Value) -> JsonRpcMessage {
        JsonRpcMessage::Request(JsonRpcRequest::new(1i64, method, Some(params)))
    }

    fn headers(message: &JsonRpcMessage) -> Vec<(&'static str, String)> {
        standard_headers(message)
    }

    /// `Mcp-Method` is required on *all* requests — its absence is what Sentry
    /// rejected with `HeaderMismatch` while ferritin accepted it silently.
    #[test]
    fn every_request_carries_its_method() {
        assert_eq!(
            headers(&request("server/discover", serde_json::json!({}))),
            [("mcp-method", "server/discover".to_string())]
        );
        assert_eq!(
            headers(&JsonRpcMessage::Notification(JsonRpcNotification::new(
                "notifications/initialized",
                None
            ))),
            [("mcp-method", "notifications/initialized".to_string())]
        );
    }

    /// `Mcp-Name` comes from `params.name` for tools and prompts, `params.uri`
    /// for resources, and from nowhere for anything else.
    #[test]
    fn only_the_three_addressed_methods_carry_a_name() {
        assert_eq!(
            headers(&request(
                "tools/call",
                serde_json::json!({"name": "get_weather", "arguments": {}})
            )),
            [
                ("mcp-method", "tools/call".to_string()),
                ("mcp-name", "get_weather".to_string()),
            ]
        );
        assert_eq!(
            mcp_name(
                "resources/read",
                Some(&serde_json::json!({"uri": "file:///projects/myapp/config.json"}))
            )
            .as_deref(),
            Some("file:///projects/myapp/config.json")
        );
        assert_eq!(
            mcp_name("prompts/get", Some(&serde_json::json!({"name": "review"}))).as_deref(),
            Some("review")
        );
        // tools/list addresses nothing.
        assert_eq!(mcp_name("tools/list", Some(&serde_json::json!({}))), None);
        // The right method with the wrong shape yields nothing rather than panicking.
        assert_eq!(mcp_name("tools/call", None), None);
        assert_eq!(
            mcp_name("tools/call", Some(&serde_json::json!({"name": 7}))),
            None
        );
    }

    /// The header must agree with the body, so a namespaced wire name must not
    /// leak into it — the server sees only its own tool's name.
    #[test]
    fn the_name_header_mirrors_the_body_verbatim() {
        let message = request(
            "tools/call",
            serde_json::json!({"name": "find_organizations", "arguments": {}}),
        );
        let name = headers(&message)
            .into_iter()
            .find(|(h, _)| *h == "mcp-name")
            .unwrap()
            .1;
        assert_eq!(name, "find_organizations");
        assert!(!name.contains("__"));
    }

    /// The encoding examples from the spec's Value Encoding table, verbatim.
    #[test]
    fn the_spec_value_encoding_examples() {
        for (original, encoded) in [
            ("Hello, 世界", "=?base64?SGVsbG8sIOS4lueVjA==?="),
            (" padded ", "=?base64?IHBhZGRlZCA=?="),
            ("line1\nline2", "=?base64?bGluZTEKbGluZTI=?="),
            ("=?base64?literal?=", "=?base64?PT9iYXNlNjQ/bGl0ZXJhbD89?="),
        ] {
            assert_eq!(encode_header_value(original), encoded, "for {original:?}");
        }
    }

    /// A value that is already safe rides raw — encoding everything would work
    /// but would make every header unreadable in a packet capture.
    #[test]
    fn safe_values_are_not_encoded() {
        for safe in [
            "tools/call",
            "get_weather",
            "file:///projects/myapp/config.json",
            "a-name_with.punctuation!",
            "",
        ] {
            assert_eq!(encode_header_value(safe), safe);
        }
    }
}
