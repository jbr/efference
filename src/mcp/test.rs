use std::sync::{Arc, Mutex};

use mcplease::types::{
    CallToolResult, JsonRpcError, JsonRpcMessage, JsonRpcRequest, JsonRpcResponse, RequestId,
    Tool as ToolDefinition,
};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, DuplexStream, ReadHalf, WriteHalf};

use super::stdio::JsonRpcPipe;
use super::*;
use crate::record::log::ToolCall;

/// The client half of an in-memory duplex — the framing and mount logic
/// without a subprocess (`Transport::Loopback`).
pub(super) type LoopbackPipe =
    JsonRpcPipe<BufReader<ReadHalf<DuplexStream>>, WriteHalf<DuplexStream>>;

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
    let pipe = JsonRpcPipe::new(BufReader::new(read), write);
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
    ToolCall::new(name, "toolu_1", input)
}

async fn run(provider: &ConnectedProvider, tool_call: &ToolCall) -> RawResult {
    run_capturing(provider, tool_call, std::future::pending())
        .await
        .0
}

/// Run a call and hand back everything the server's traffic reached: the
/// result, the live-output lines, and the inbox notifications.
async fn run_capturing(
    provider: &ConnectedProvider,
    tool_call: &ToolCall,
    abort: impl Future<Output = Interrupt> + Send,
) -> (RawResult, Vec<String>, Vec<Notification>) {
    let output = Arc::new(Mutex::new(Vec::new()));
    let notifications = Arc::new(Mutex::new(Vec::new()));
    let (seen_output, seen_notifications) = (Arc::clone(&output), Arc::clone(&notifications));
    let outcome = provider
        .run(
            tool_call,
            abort,
            move |chunk: &[u8]| {
                seen_output
                    .lock()
                    .unwrap()
                    .push(String::from_utf8_lossy(chunk).into_owned());
            },
            move |notification| seen_notifications.lock().unwrap().push(notification),
        )
        .await;
    let result = match outcome {
        Outcome::Done(result) => result,
        Outcome::Detached(_) => panic!("a connected call never detaches"),
    };
    (
        result,
        output.lock().unwrap().clone(),
        notifications.lock().unwrap().clone(),
    )
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

/// `GET /v1/tools/:name` (docs/control-api.md) attributes a mounted tool to
/// the server that advertised it — `ToolEntry::server`, read off `Toolbox`
/// over a real (loopback) mount rather than a hand-built fixture.
#[tokio::test]
async fn a_mounted_tools_entry_names_its_server() {
    let (transport, _) = loopback(scripted_server());
    let provider = ConnectedProvider::mount_transport("fake", transport)
        .await
        .unwrap();
    let toolbox =
        crate::tools::Toolbox::new(vec![crate::tools::Provider::Connected(Box::new(provider))]);

    let entry = toolbox.tool_entry("fake__zeta").expect("mounted");
    assert_eq!(entry.server.as_deref(), Some("fake"));
    assert_eq!(entry.description.as_deref(), Some("tool zeta"));
    assert_eq!(
        entry.input_schema["properties"]["x"]["type"],
        json!("string")
    );

    // `GET /v1/tools/:name` resolves the dotted operator spelling
    // (docs/control-api.md) through the same `wire_tool_name` normalization
    // the config layer applies to a `[tools]` grant.
    let dotted_wire = crate::config::wire_tool_name("fake.zeta".to_string());
    assert_eq!(dotted_wire, "fake__zeta");
    let via_dotted = toolbox.tool_entry(&dotted_wire).expect("resolved");
    assert_eq!(via_dotted.server.as_deref(), Some("fake"));

    assert!(
        toolbox.tool_entry("fake__missing").is_none(),
        "an unmounted name under a real server is still a clean absence"
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
            let caller = context.client_info().expect("clientInfo in _meta");
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
        &McpTransport::Http {
            url: "https://ferritin.rs/mcp".into(),
        },
        Path::new("."),
        None,
        &crate::env_table::ChildEnv::default(),
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
        &McpTransport::Stdio {
            command: "cargo-mcp".into(),
            args: vec!["serve".into()],
            env: [("HOME".to_string(), home.display().to_string())].into(),
        },
        Path::new("."),
        None,
        &crate::env_table::ChildEnv::default(),
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

/// An image block is realized into `RawResult::images` with a header line in
/// the text; an undecodable one degrades to a placeholder naming the failure;
/// a structured-only result lands as pretty JSON.
#[test]
fn image_blocks_realize_and_structured_content_renders_as_json() {
    use base64::Engine;
    let img = image::ImageBuffer::from_fn(100, 60, |x, y| {
        image::Rgb([(x % 256) as u8, (y % 256) as u8, 128u8])
    });
    let mut png = Vec::new();
    image::DynamicImage::ImageRgb8(img)
        .write_with_encoder(image::codecs::png::PngEncoder::new(&mut png))
        .unwrap();
    let result = CallToolResult {
        content: vec![
            ContentBlock::text("plain"),
            ContentBlock::Image {
                data: base64::engine::general_purpose::STANDARD.encode(&png),
                mime_type: "image/png".into(),
                annotations: None,
                meta: None,
            },
        ],
        ..CallToolResult::default()
    };
    let rendered = render_result(&result);
    assert_eq!(
        rendered.content,
        "plain\n\n100×60 image/png, 12 visual tokens"
    );
    assert!(!rendered.is_error);
    assert_eq!(rendered.images.len(), 1);
    assert_eq!(rendered.images[0].media_type, "image/png");
    assert_eq!(
        (rendered.images[0].width, rendered.images[0].height),
        (100, 60)
    );

    // Not an image: `aGVsbG8=` decodes to "hello". The block degrades to a
    // placeholder and nothing lands in `images`.
    let undecodable = CallToolResult {
        content: vec![ContentBlock::Image {
            data: "aGVsbG8=".into(),
            mime_type: "image/png".into(),
            annotations: None,
            meta: None,
        }],
        ..CallToolResult::default()
    };
    let rendered = render_result(&undecodable);
    assert!(
        rendered
            .content
            .starts_with("[image image/png (8 base64 bytes) — could not be rendered:"),
        "got {:?}",
        rendered.content
    );
    assert!(rendered.images.is_empty());

    let structured_only = CallToolResult {
        structured_content: Some(json!({ "answer": 42 })),
        ..CallToolResult::default()
    };
    let rendered = render_result(&structured_only);
    assert!(rendered.content.contains("\"answer\": 42"));
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

#[tokio::test]
async fn progress_ticks_stream_as_live_output_and_stay_out_of_the_result() {
    let script: Box<Script> = Box::new(|request: &JsonRpcRequest| match request.method.as_str() {
        "server/discover" => declines_discovery(request),
        "initialize" => vec![JsonRpcMessage::Response(JsonRpcResponse::success(
            request.id.clone(),
            initialize_result("2025-06-18"),
        ))],
        "tools/list" => vec![JsonRpcMessage::Response(JsonRpcResponse::success(
            request.id.clone(),
            json!({ "tools": [] }),
        ))],
        "tools/call" => {
            let tick = |params: Value| {
                JsonRpcMessage::Notification(mcplease::types::JsonRpcNotification::new(
                    "notifications/progress",
                    Some(params),
                ))
            };
            vec![
                tick(json!({
                    "progressToken": request.id.clone(),
                    "progress": 12,
                    "total": 40,
                    "message": "still indexing"
                })),
                tick(json!({ "progressToken": request.id.clone(), "progress": 30 })),
                JsonRpcMessage::Response(JsonRpcResponse::success(
                    request.id.clone(),
                    serde_json::to_value(CallToolResult::text("done")).unwrap(),
                )),
            ]
        }
        other => panic!("unexpected method {other}"),
    });
    let (transport, _) = loopback(script);
    let provider = ConnectedProvider::mount_transport("fake", transport)
        .await
        .unwrap();
    let (result, output, notifications) = run_capturing(
        &provider,
        &call("fake__zeta", json!({})),
        std::future::pending(),
    )
    .await;

    assert!(!result.is_error, "{}", result.content);
    // The result is the tool's answer alone; the ticks are pre-commit live
    // output (docs/tools.md "the client protocol").
    assert_eq!(result.content, "done");
    assert_eq!(
        output,
        vec![
            "[progress 12/40] still indexing\n".to_string(),
            "[progress 30]\n".to_string()
        ]
    );
    assert!(notifications.is_empty());
}

#[tokio::test]
async fn server_messages_are_enqueued_on_the_servers_slot() {
    let script: Box<Script> = Box::new(|request: &JsonRpcRequest| match request.method.as_str() {
        "server/discover" => declines_discovery(request),
        "initialize" => vec![JsonRpcMessage::Response(JsonRpcResponse::success(
            request.id.clone(),
            initialize_result("2025-06-18"),
        ))],
        "tools/list" => vec![JsonRpcMessage::Response(JsonRpcResponse::success(
            request.id.clone(),
            json!({ "tools": [] }),
        ))],
        "tools/call" => vec![
            JsonRpcMessage::Notification(mcplease::types::JsonRpcNotification::new(
                "notifications/message",
                Some(json!({ "level": "warning", "logger": "indexer", "data": "ccls is slow" })),
            )),
            JsonRpcMessage::Response(JsonRpcResponse::success(
                request.id.clone(),
                serde_json::to_value(CallToolResult::text("done")).unwrap(),
            )),
        ],
        other => panic!("unexpected method {other}"),
    });
    let (transport, _) = loopback(script);
    let provider = ConnectedProvider::mount_transport("fake", transport)
        .await
        .unwrap();
    let (result, output, notifications) = run_capturing(
        &provider,
        &call("fake__zeta", json!({})),
        std::future::pending(),
    )
    .await;

    assert!(!result.is_error, "{}", result.content);
    assert!(output.is_empty());
    assert_eq!(notifications.len(), 1);
    let message = &notifications[0];
    assert_eq!(message.source, "fake");
    assert_eq!(message.body, "[warning] indexer: ccls is slow");
    assert_eq!(message.slot.as_deref(), Some("mcp:fake"));
}

#[tokio::test(start_paused = true)]
async fn an_abandoned_call_is_cancelled_on_the_wire() {
    let call_id = Arc::new(Mutex::new(None::<RequestId>));
    let record = Arc::clone(&call_id);
    let script: Box<Script> =
        Box::new(
            move |request: &JsonRpcRequest| match request.method.as_str() {
                "server/discover" => declines_discovery(request),
                "initialize" => vec![JsonRpcMessage::Response(JsonRpcResponse::success(
                    request.id.clone(),
                    initialize_result("2025-06-18"),
                ))],
                "tools/list" => vec![JsonRpcMessage::Response(JsonRpcResponse::success(
                    request.id.clone(),
                    json!({ "tools": [] }),
                ))],
                // A call that never answers: the operator gives up on it.
                "tools/call" => {
                    *record.lock().unwrap() = Some(request.id.clone());
                    vec![]
                }
                other => panic!("unexpected method {other}"),
            },
        );
    let (transport, inbox) = loopback(script);
    let provider = ConnectedProvider::mount_transport("fake", transport)
        .await
        .unwrap();
    // Resolved well after the request has gone out (its first poll sends it),
    // so the cancellation is about a call the server has actually seen.
    let abort = async {
        tokio::time::sleep(Duration::from_millis(50)).await;
        Interrupt::Kill
    };
    let (result, _, _) = run_capturing(&provider, &call("fake__zeta", json!({})), abort).await;

    assert!(result.is_error, "{}", result.content);
    assert!(
        result.content.contains("cancelled by the operator"),
        "got {:?}",
        result.content
    );
    let expect = match call_id.lock().unwrap().clone().unwrap() {
        RequestId::Integer(n) => n,
        other => panic!("expected an integer id, got {other:?}"),
    };
    // The server is told which request to stop. Its reader task records the
    // notification asynchronously, so wait for it.
    let mut cancelled = false;
    for _ in 0..200 {
        cancelled = inbox.lock().unwrap().iter().any(|message| {
            matches!(
                message,
                JsonRpcMessage::Notification(n)
                    if n.method == "notifications/cancelled"
                        && n.params
                            .as_ref()
                            .and_then(|params| params.get("requestId"))
                            .and_then(Value::as_i64)
                            == Some(expect)
            )
        });
        if cancelled {
            break;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    assert!(cancelled, "no cancellation named the abandoned call");
}
