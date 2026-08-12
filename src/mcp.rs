//! Connected MCP providers (docs/tools.md) — user-declared servers efference
//! did not compile in, reached over a wire instead of a function call.
//!
//! A connected provider is the dynamic counterpart of the embedded
//! [`crate::tools::McpServer`]: its tool table is discovered at runtime via
//! `tools/list` rather than known at compile time, and `tools/call` dispatches
//! over a transport (a spawned stdio subprocess or a streamable-HTTP endpoint)
//! instead of through a typed enum. Servers are declared in the config
//! (`[mcp.servers.<name>]`, [`crate::config::McpTransport`]); declaring one is
//! the human's authority, and calling its tools rides the same gate cascade as
//! every other tool.
//!
//! The MCP `client`/`server` role names describe wire direction only and stay
//! inside this module; efference's vocabulary is *host mounts provider*.
//!
//! Cache discipline (docs/log-and-projection.md): the tool table is a
//! **snapshot at mount** — discovered once, sorted by tool name (a server may
//! reorder its list between sessions), folded through the same
//! `anthropic_def` normalization as the embedded servers, and frozen for the
//! session. A server that changes its tools mid-session does not reach the
//! wire; re-mount (a config change) is the boundary where a changed table may
//! land.
//!
//! Tool names are namespaced `<server>__<tool>` on the wire, which makes
//! collisions across servers structurally impossible (embedded tool names
//! contain no `__`, and the config validates server names against the API's
//! tool-name alphabet).
//!
//! Protocol interop: requests open with the stateful `initialize` handshake
//! (revisions through `2025-11-25` — what deployed servers speak). The
//! `2026-07-28` revision replaces the handshake with stateless per-request
//! `_meta` and `server/discover`; the mcplease types model both, and the
//! handshake here accepts any supported revision the server negotiates down
//! to. A stateless-only server is not yet reachable — that flow lands when a
//! real one exists to test against.

use std::future::Future;
use std::path::Path;
use std::time::Duration;

use mcplease::types::{
    CallToolResult, ClientCapabilities, ContentBlock, Implementation, InitializeRequestParams,
    InitializeResult, ListToolsResult, ResourceContents, SUPPORTED_PROTOCOL_VERSIONS,
    Tool as ToolDefinition,
};
use serde_json::{Value, json};
use tokio::sync::Mutex;

use crate::bash::Outcome;
use crate::config::McpTransport;
use crate::frontend::Interrupt;
use crate::log::{RawResult, ToolCall};
use crate::tools::anthropic_def;

mod http;
mod stdio;

#[cfg(test)]
mod test;

/// The separator between server name and tool name on the wire
/// (`github__create_issue`). Two underscores: legal in the API's tool-name
/// alphabet, and absent from every embedded tool name, so a namespaced name
/// can never collide with an embedded one.
pub const NAMESPACE_SEPARATOR: &str = "__";

/// The protocol revision requested in the `initialize` handshake. The newest
/// handshake revision — a server on an older revision answers with its own,
/// and any supported answer is accepted.
const REQUESTED_PROTOCOL_VERSION: &str = mcplease::types::LATEST_HANDSHAKE_PROTOCOL_VERSION;

/// How long a server gets to complete handshake + discovery before the mount
/// fails. Individual tool calls are not timed out — the operator's interrupt
/// covers a hung call.
const MOUNT_TIMEOUT: Duration = Duration::from_secs(30);

/// A mounted connected provider: a live connection plus the tool-table
/// snapshot taken at mount.
pub struct ConnectedProvider {
    name: String,
    /// Namespaced wire names, aligned with `definitions`.
    names: Vec<String>,
    /// Anthropic `tools[]` entries — namespaced, normalized, frozen at mount.
    definitions: Vec<Value>,
    /// The live connection. An async-aware mutex (not `RefCell`) because a
    /// `tools/call` holds it across awaits; the loop is single-threaded, so
    /// there is no contention to speak of.
    transport: Mutex<Transport>,
}

impl ConnectedProvider {
    /// Connect to a declared server, run the handshake, and snapshot its tool
    /// table. `cwd` is the working directory a stdio server is spawned in.
    pub async fn mount(name: &str, config: &McpTransport, cwd: &Path) -> anyhow::Result<Self> {
        let transport = match config {
            McpTransport::Stdio { command, args, env } => {
                Transport::Stdio(stdio::StdioTransport::spawn(command, args, env, cwd)?)
            }
            McpTransport::Http { url } => Transport::Http(http::HttpTransport::new(url)?),
        };
        Self::mount_transport(name, transport).await
    }

    /// Handshake + discovery over an already-open transport (the seam the
    /// loopback tests use).
    async fn mount_transport(name: &str, mut transport: Transport) -> anyhow::Result<Self> {
        let tools = tokio::time::timeout(MOUNT_TIMEOUT, async {
            handshake(&mut transport).await?;
            list_tools(&mut transport).await
        })
        .await
        .map_err(|_| {
            anyhow::anyhow!("server did not finish handshake + discovery within {MOUNT_TIMEOUT:?}")
        })??;

        // Sort by tool name: the snapshot must yield byte-identical
        // definitions across sessions even if the server reorders its list.
        let mut tools = tools;
        tools.sort_by(|a, b| a.name.cmp(&b.name));

        let names: Vec<String> = tools
            .iter()
            .map(|tool| format!("{name}{NAMESPACE_SEPARATOR}{}", tool.name))
            .collect();
        let definitions = tools
            .iter()
            .zip(&names)
            .map(|(tool, wire_name)| {
                let mut namespaced = tool.clone();
                namespaced.name = wire_name.clone();
                anthropic_def(&namespaced)
            })
            .collect();

        Ok(Self {
            name: name.to_string(),
            names,
            definitions,
            transport: Mutex::new(transport),
        })
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    /// This provider's wire tool definitions (`tools[]` entries).
    pub fn definitions(&self) -> Vec<Value> {
        self.definitions.clone()
    }

    /// Whether this provider owns the (namespaced) tool name.
    pub fn handles(&self, name: &str) -> bool {
        self.names.iter().any(|n| n == name)
    }

    /// Run one call. Races the operator's interrupt: `Kill` abandons the call
    /// and pairs the `tool_use` with an error result (a stale response
    /// arriving later is skipped by id); `Detach` is bash-only and keeps
    /// waiting.
    pub async fn run(&self, call: &ToolCall, abort: impl Future<Output = Interrupt>) -> Outcome {
        let mut call_future = std::pin::pin!(self.call(&call.name, &call.input));
        let abort = std::pin::pin!(abort);
        tokio::select! {
            result = &mut call_future => result.into(),
            interrupt = abort => match interrupt {
                Interrupt::Kill => RawResult {
                    content: format!("`{}` call cancelled by the operator", call.name),
                    is_error: true,
                }
                .into(),
                Interrupt::Detach => call_future.await.into(),
            },
        }
    }

    /// `tools/call` over the transport. Any failure — transport, protocol
    /// error, unparseable result — lands as an `is_error` result so the
    /// `tool_use` always gets its pair (never an orphan).
    async fn call(&self, wire_name: &str, input: &Value) -> RawResult {
        let prefix = format!("{}{NAMESPACE_SEPARATOR}", self.name);
        let tool = wire_name.strip_prefix(&prefix).unwrap_or(wire_name);
        let params = json!({ "name": tool, "arguments": input });
        let mut transport = self.transport.lock().await;
        match transport.request("tools/call", Some(params)).await {
            Ok(result) => match serde_json::from_value::<CallToolResult>(result) {
                Ok(result) => render_result(result),
                Err(e) => RawResult {
                    content: format!("`{}` returned an unparseable tool result: {e}", self.name),
                    is_error: true,
                },
            },
            Err(e) => RawResult {
                content: format!("`{}` call failed: {e}", self.name),
                is_error: true,
            },
        }
    }
}

/// A live connection to a server, whichever transport the config declared.
enum Transport {
    Stdio(stdio::StdioTransport),
    Http(http::HttpTransport),
    /// An in-memory pipe pair for tests: the framing and mount logic without
    /// a subprocess.
    #[cfg(test)]
    Loopback(test::LoopbackPipe),
}

impl Transport {
    /// One request-response round trip. `Ok` is the JSON-RPC `result`;
    /// protocol errors come back as `Err`.
    async fn request(&mut self, method: &str, params: Option<Value>) -> anyhow::Result<Value> {
        match self {
            Transport::Stdio(t) => t.request(method, params).await,
            Transport::Http(t) => t.request(method, params).await,
            #[cfg(test)]
            Transport::Loopback(t) => t.request(method, params).await,
        }
    }

    /// Send a one-way notification.
    async fn notify(&mut self, method: &str, params: Option<Value>) -> anyhow::Result<()> {
        match self {
            Transport::Stdio(t) => t.notify(method, params).await,
            Transport::Http(t) => t.notify(method, params).await,
            #[cfg(test)]
            Transport::Loopback(t) => t.notify(method, params).await,
        }
    }

    /// Record the negotiated protocol version (the HTTP transport carries it
    /// as a header on every subsequent request; stdio needs nothing).
    fn set_protocol_version(&mut self, version: &str) {
        match self {
            Transport::Stdio(_) => {}
            Transport::Http(t) => t.set_protocol_version(version),
            #[cfg(test)]
            Transport::Loopback(_) => {}
        }
    }
}

/// The stateful `initialize` handshake: send our identity and requested
/// revision, accept whichever supported revision the server negotiates, and
/// confirm with `notifications/initialized`.
async fn handshake(transport: &mut Transport) -> anyhow::Result<InitializeResult> {
    let params = InitializeRequestParams {
        protocol_version: REQUESTED_PROTOCOL_VERSION.into(),
        capabilities: ClientCapabilities::default(),
        client_info: Implementation::new("efference", env!("CARGO_PKG_VERSION")),
    };
    let result = transport
        .request("initialize", Some(serde_json::to_value(&params)?))
        .await?;
    let init: InitializeResult = serde_json::from_value(result)
        .map_err(|e| anyhow::anyhow!("unparseable initialize result: {e}"))?;
    if !SUPPORTED_PROTOCOL_VERSIONS.contains(&init.protocol_version.as_str()) {
        anyhow::bail!(
            "server negotiated protocol version `{}`, which efference does not speak",
            init.protocol_version
        );
    }
    transport.set_protocol_version(&init.protocol_version);
    transport.notify("notifications/initialized", None).await?;
    Ok(init)
}

/// Drain the server's full `tools/list`, following pagination cursors.
async fn list_tools(transport: &mut Transport) -> anyhow::Result<Vec<ToolDefinition>> {
    let mut tools = Vec::new();
    let mut cursor: Option<String> = None;
    loop {
        let params = cursor.take().map(|cursor| json!({ "cursor": cursor }));
        let result = transport.request("tools/list", params).await?;
        let page: ListToolsResult = serde_json::from_value(result)
            .map_err(|e| anyhow::anyhow!("unparseable tools/list result: {e}"))?;
        tools.extend(page.tools);
        match page.next_cursor {
            Some(next) => cursor = Some(next),
            None => return Ok(tools),
        }
    }
}

/// Fold a `CallToolResult` into the loop's [`RawResult`]. Text blocks land
/// verbatim; non-text blocks land as placeholders naming what was withheld
/// (the log stays a faithful record that *something* was there); a
/// structured-only result lands as pretty JSON.
fn render_result(result: CallToolResult) -> RawResult {
    let is_error = result.is_error.unwrap_or(false);
    let mut parts: Vec<String> = result.content.iter().map(render_block).collect();
    if parts.is_empty()
        && let Some(structured) = &result.structured_content
    {
        parts.push(serde_json::to_string_pretty(structured).unwrap_or_default());
    }
    if parts.is_empty() {
        parts.push("(empty result)".to_string());
    }
    RawResult {
        content: parts.join("\n\n"),
        is_error,
    }
}

fn render_block(block: &ContentBlock) -> String {
    match block {
        ContentBlock::Text { text, .. } => text.clone(),
        ContentBlock::Image {
            mime_type, data, ..
        } => format!(
            "[image {mime_type} ({} base64 bytes) — not rendered]",
            data.len()
        ),
        ContentBlock::Audio {
            mime_type, data, ..
        } => format!(
            "[audio {mime_type} ({} base64 bytes) — not rendered]",
            data.len()
        ),
        ContentBlock::ResourceLink {
            uri, description, ..
        } => match description {
            Some(description) => format!("[resource: {uri} — {description}]"),
            None => format!("[resource: {uri}]"),
        },
        ContentBlock::EmbeddedResource { resource, .. } => match resource {
            ResourceContents::Text { uri, text, .. } => format!("{uri}:\n{text}"),
            ResourceContents::Blob { uri, mime_type, .. } => match mime_type {
                Some(mime_type) => format!("[binary resource {uri} ({mime_type}) — not rendered]"),
                None => format!("[binary resource {uri} — not rendered]"),
            },
        },
    }
}
