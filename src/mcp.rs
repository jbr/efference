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
//! tool-name alphabet). That alphabet (`[a-zA-Z0-9_-]`) has no separator that
//! reads as one — the MCP spec permits `.`, the provider does not — so
//! [`display_name`] renders the boundary as a dot for the TUI.
//!
//! Protocol interop is `mcplease::client`'s (docs/tools.md "the client
//! protocol"): `ClientProtocol` builds each request and classifies each
//! arriving message, and `Negotiation` runs the `server/discover` probe that
//! `2026-07-28` introduced, falling back to the stateful `initialize`
//! handshake that deployed servers still speak. Neither does any I/O, so what
//! is left here is one loop — send a request, read frames until its response
//! arrives — and the two transports are pure framing beneath it.

use std::future::Future;
use std::path::Path;
use std::time::Duration;

use mcplease::client::{ClientProtocol, Negotiation, Reaction, Step};
use mcplease::types::{
    CallToolResult, ContentBlock, Implementation, InputRequiredResult, JsonRpcError,
    JsonRpcMessage, JsonRpcRequest, ListToolsResult, ResourceContents, Tool as ToolDefinition,
    ToolCallOutcome,
};
use serde_json::Value;
use tokio::sync::Mutex;

use crate::bash::Outcome;
use crate::config::McpTransport;
use crate::frontend::Interrupt;
use crate::log::{RawResult, ToolCall};
use crate::tools::anthropic_def;

pub mod auth;
mod http;
mod stdio;

#[cfg(test)]
mod test;

/// The separator between server name and tool name on the wire
/// (`github__create_issue`). Two underscores: legal in the API's tool-name
/// alphabet, and absent from every embedded tool name, so a namespaced name
/// can never collide with an embedded one.
pub const NAMESPACE_SEPARATOR: &str = "__";

/// A namespaced wire name rendered for a human: `ferritin__get` → `ferritin.get`.
///
/// The wire alphabet (`[a-zA-Z0-9_-]`) has no separator that reads as one, so
/// `__` is what the name must *be*; a dot is what it wants to look like. Only
/// the first occurrence is replaced — the server name is validated against that
/// same alphabet and embedded tool names contain no `__`, so the first `__` is
/// always the namespace boundary and any later one belongs to the server's own
/// tool name.
///
/// Display only. Nothing that reaches the wire, the log, or a lookup may go
/// through here — the dotted form is not a name anything can be found by.
pub fn display_name(wire_name: &str) -> String {
    wire_name.replacen(NAMESPACE_SEPARATOR, ".", 1)
}

/// A declared server that did not mount, and why (docs/tools.md).
///
/// Only `optional = true` servers can reach here — a required one that fails
/// stops the boot — but the session still has to account for it, in two
/// directions. The operator is told at every launch, because a config entry
/// that quietly does nothing is a standing papercut. The model is told in the
/// preamble, because otherwise it reads its tool table as the whole truth and
/// answers "I can't do that" to a request the operator's config says it can.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unmounted {
    pub name: String,
    /// Why it did not mount, rendered for a reader: an uninstalled command, an
    /// unreachable host, a TLS failure, a server awaiting authorization, a
    /// server writing non-JSON to stdout. The config collapses all of these to
    /// one flag; the distinction lives here, where it can be acted on.
    pub reason: String,
}

/// The preamble section naming what the operator declared and this session does
/// not have. `None` when everything mounted — a heading explaining that nothing
/// is missing is noise in the cached prefix.
///
/// Frozen into the system prompt at boot like every other preamble section, so
/// it stays byte-stable for the session (docs/log-and-projection.md).
pub fn unmounted_section(unmounted: &[Unmounted]) -> Option<String> {
    if unmounted.is_empty() {
        return None;
    }
    let mut section = String::from(
        "# Declared but unavailable\n\nThese MCP servers are declared in this session's \
         configuration but did not connect, so their tools are absent from your tool list. \
         If asked for something one of them provides, say that it is configured but \
         unavailable and give the reason — do not answer as though the capability does not \
         exist.\n",
    );
    for Unmounted { name, reason } in unmounted {
        section.push_str(&format!("\n- `{name}` — {reason}"));
    }
    section
        .push_str("\n\nThe operator was told the same thing at startup; fixing it is their call.");
    Some(section)
}

/// How long a server gets to complete negotiation + discovery before the mount
/// fails. Individual tool calls are not timed out — the operator's interrupt
/// covers a hung call.
const MOUNT_TIMEOUT: Duration = Duration::from_secs(30);

/// A mounted connected provider: a live connection plus the tool-table
/// snapshot taken at mount.
pub struct ConnectedProvider {
    name: String,
    /// The server's tool table as discovered, with each `name` rewritten to its
    /// namespaced form — aligned with `definitions`. Held whole rather than as a
    /// name list because `anthropic_def` keeps only what the Anthropic tool shape
    /// has a slot for; the server's own `annotations` and `title` are dropped
    /// there and read here instead (`crate::tools::ToolBrief`,
    /// docs/permissions.md).
    tools: Vec<ToolDefinition>,
    /// Anthropic `tools[]` entries — namespaced, normalized, frozen at mount.
    definitions: Vec<Value>,
    /// The live connection. An async-aware mutex (not `RefCell`) because a
    /// `tools/call` holds it across awaits; the loop is single-threaded, so
    /// there is no contention to speak of.
    session: Mutex<Session>,
}

impl ConnectedProvider {
    /// Connect to a declared server, negotiate, and snapshot its tool table.
    /// `cwd` is the working directory a stdio server is spawned in.
    pub async fn mount(
        name: &str,
        config: &McpTransport,
        cwd: &Path,
        authorization: Option<&str>,
    ) -> anyhow::Result<Self> {
        let transport = match config {
            McpTransport::Stdio { command, args, env } => {
                // Stdio servers take credentials from the environment by the
                // spec's own direction, so an `Authorization` header has no
                // meaning here even if one was somehow stored.
                Transport::Stdio(stdio::StdioTransport::spawn(command, args, env, cwd)?)
            }
            McpTransport::Http { url } => {
                Transport::Http(http::HttpTransport::new(url)?.with_authorization(authorization))
            }
        };
        Self::mount_transport(name, transport).await
    }

    /// Negotiation + discovery over an already-open transport (the seam the
    /// loopback tests use).
    async fn mount_transport(name: &str, transport: Transport) -> anyhow::Result<Self> {
        let mut session = Session::new(transport);
        let tools = tokio::time::timeout(MOUNT_TIMEOUT, async {
            session.negotiate().await?;
            session.list_tools().await
        })
        .await
        .map_err(|_| {
            anyhow::anyhow!(
                "server did not finish negotiation + discovery within {MOUNT_TIMEOUT:?}"
            )
        })??;

        // Sort by tool name: the snapshot must yield byte-identical
        // definitions across sessions even if the server reorders its list.
        let mut tools = tools;
        tools.sort_by(|a, b| a.name.cmp(&b.name));

        // Namespace in place, once: the snapshot and the wire definitions are then
        // two views of the same entries, and `handles` / `brief` / `tools[]` can
        // never disagree about a name.
        let tools: Vec<ToolDefinition> = tools
            .into_iter()
            .map(|mut tool| {
                tool.name = format!("{name}{NAMESPACE_SEPARATOR}{}", tool.name);
                tool
            })
            .collect();
        let definitions = tools.iter().map(anthropic_def).collect();

        Ok(Self {
            name: name.to_string(),
            tools,
            definitions,
            session: Mutex::new(session),
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
        self.tools.iter().any(|t| t.name == name)
    }

    /// The spotter's brief for one of this server's tools
    /// (docs/permissions.md) — the snapshotted entry, tagged with the server it
    /// came from.
    pub fn brief(&self, name: &str) -> Option<crate::tools::ToolBrief<'_>> {
        self.tools
            .iter()
            .find(|t| t.name == name)
            .map(|definition| crate::tools::ToolBrief {
                server: Some(&self.name),
                definition,
            })
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
        let mut session = self.session.lock().await;
        let request = session.protocol.tools_call(tool, input);
        // Two failure layers flatten to one: the connection or framing giving
        // out, and the server answering with a protocol error. Both are the
        // call failing, and both must land as a paired result.
        let result = match session.round_trip(request).await {
            Ok(Ok(result)) => Ok(result),
            Ok(Err(protocol)) => Err(anyhow::Error::new(protocol)),
            Err(transport) => Err(transport),
        };
        match result {
            Ok(result) => match serde_json::from_value::<ToolCallOutcome>(result) {
                Ok(ToolCallOutcome::Complete(result)) => render_result(result),
                // An interim result: the server wants more input (elicitation,
                // sampling) or is shedding load, and expects the client to
                // supply it and re-issue. efference declares no client
                // capabilities and does not retry, so this lands as an error
                // naming what the server asked for — better than the empty
                // `content` a `CallToolResult` parse would have produced.
                Ok(ToolCallOutcome::InputRequired(interim)) => RawResult {
                    content: format!(
                        "`{}` returned an interim result asking for more input ({}), which \
                         efference does not supply",
                        self.name,
                        describe_input_required(&interim),
                    ),
                    is_error: true,
                },
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

/// A connection and the protocol state riding on it.
///
/// [`ClientProtocol`] decides *what* to send and what each arriving message
/// means; [`Transport`] moves bytes. [`round_trip`](Session::round_trip) is
/// the only place the two meet, and it is the same loop for every transport —
/// which is what keeps stdio and HTTP from each growing their own copy of the
/// protocol's rules.
struct Session {
    protocol: ClientProtocol,
    transport: Transport,
}

impl Session {
    fn new(transport: Transport) -> Self {
        Self {
            // No optional capabilities: efference answers no server-initiated
            // elicitation or sampling, and says so on every request rather
            // than letting a server infer it.
            protocol: ClientProtocol::new(Implementation::new(
                "efference",
                env!("CARGO_PKG_VERSION"),
            )),
            transport,
        }
    }

    /// Send one request and read frames until its response arrives. `Ok(Err)`
    /// is the server answering with a protocol error; `Err` is the connection
    /// or the framing failing.
    async fn round_trip(
        &mut self,
        request: JsonRpcRequest,
    ) -> anyhow::Result<Result<Value, JsonRpcError>> {
        let id = request.id.clone();
        let mut exchange = self
            .transport
            .open(&JsonRpcMessage::Request(request))
            .await?;
        loop {
            let frame = exchange.next_frame().await?;
            match self.protocol.on_message(frame) {
                Reaction::Result { id: got, result } if got == id => return Ok(result),
                // A response to a call the operator abandoned. Its `tool_use`
                // was already paired with the cancellation.
                Reaction::Result { id, .. } => {
                    log::debug!("skipping response to abandoned request {id:?}");
                }
                Reaction::Reply(response) => {
                    exchange.reply(&JsonRpcMessage::Response(response)).await?;
                }
                Reaction::Ignore => {}
            }
        }
    }

    /// Settle the protocol revision: the `server/discover` probe, falling back
    /// to the `initialize` handshake when the server does not have it, and
    /// confirming a handshake with `notifications/initialized`.
    async fn negotiate(&mut self) -> anyhow::Result<()> {
        let (mut negotiation, mut request) = Negotiation::start(&mut self.protocol);
        let negotiated = loop {
            let result = self.round_trip(request).await?;
            match negotiation.on_result(&mut self.protocol, result)? {
                Step::Send(next) => request = next,
                Step::Done(negotiated) => break *negotiated,
            }
        };
        // The HTTP transport carries the revision as a header on every later
        // request; stdio needs nothing.
        self.transport
            .set_protocol_version(&negotiated.protocol_version);
        if let Some(confirm) = negotiated.initialized_notification() {
            self.transport
                .notify(&JsonRpcMessage::Notification(confirm))
                .await?;
        }
        log::debug!(
            "negotiated {} ({})",
            negotiated.protocol_version,
            if negotiated.stateful {
                "handshake"
            } else {
                "stateless"
            }
        );
        Ok(())
    }

    /// Drain the server's full `tools/list`, following pagination cursors.
    async fn list_tools(&mut self) -> anyhow::Result<Vec<ToolDefinition>> {
        let mut tools = Vec::new();
        let mut cursor: Option<String> = None;
        loop {
            let request = self.protocol.tools_list(cursor.as_deref());
            let result = self.round_trip(request).await??;
            let page: ListToolsResult = serde_json::from_value(result)
                .map_err(|e| anyhow::anyhow!("unparseable tools/list result: {e}"))?;
            tools.extend(page.tools);
            match page.next_cursor {
                Some(next) => cursor = Some(next),
                None => return Ok(tools),
            }
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
    /// Send a message that expects an answer, and hand back the frames the
    /// server sends in response.
    async fn open(&mut self, message: &JsonRpcMessage) -> anyhow::Result<Exchange<'_>> {
        match self {
            Transport::Stdio(t) => {
                let pipe = t.pipe();
                pipe.send(message).await?;
                Ok(Exchange::Pipe(pipe))
            }
            Transport::Http(t) => Ok(Exchange::Http(t.open(message).await?)),
            #[cfg(test)]
            Transport::Loopback(t) => {
                t.send(message).await?;
                Ok(Exchange::Loopback(t))
            }
        }
    }

    /// Send a one-way message.
    async fn notify(&mut self, message: &JsonRpcMessage) -> anyhow::Result<()> {
        match self {
            Transport::Stdio(t) => t.pipe().send(message).await,
            Transport::Http(t) => t.notify(message).await,
            #[cfg(test)]
            Transport::Loopback(t) => t.send(message).await,
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

/// The frames one request produced. A pipe carries every exchange, so its
/// frames come off the shared connection; an HTTP exchange owns the response
/// to its own POST.
enum Exchange<'a> {
    Pipe(&'a mut stdio::ChildPipe),
    Http(http::HttpExchange),
    #[cfg(test)]
    Loopback(&'a mut test::LoopbackPipe),
}

impl Exchange<'_> {
    async fn next_frame(&mut self) -> anyhow::Result<JsonRpcMessage> {
        match self {
            Exchange::Pipe(pipe) => pipe.next_frame().await,
            Exchange::Http(exchange) => exchange.next_frame().await,
            #[cfg(test)]
            Exchange::Loopback(pipe) => pipe.next_frame().await,
        }
    }

    /// Answer a server-initiated request. A per-request SSE stream is not a
    /// channel we can write to, so an HTTP exchange drops the answer — the
    /// server learns nothing, which is the same as the honest refusal it
    /// would have received.
    async fn reply(&mut self, message: &JsonRpcMessage) -> anyhow::Result<()> {
        match self {
            Exchange::Pipe(pipe) => pipe.send(message).await,
            Exchange::Http(_) => {
                log::debug!("dropping a reply: an HTTP exchange has no channel back");
                Ok(())
            }
            #[cfg(test)]
            Exchange::Loopback(pipe) => pipe.send(message).await,
        }
    }
}

/// Fold a `CallToolResult` into the loop's [`RawResult`]. Text blocks land
/// verbatim; non-text blocks land as placeholders naming what was withheld
/// (the log stays a faithful record that *something* was there); a
/// structured-only result lands as pretty JSON.
pub(crate) fn render_result(result: CallToolResult) -> RawResult {
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

/// Name what an interim result is waiting on, for the error text the model
/// reads. The request kinds are the keys' values (`elicitation/create`,
/// `sampling/createMessage`, `roots/list`); a result with only
/// `request_state` is the spec's load-shedding case — the server is asking to
/// be retried, not asking a question.
fn describe_input_required(interim: &InputRequiredResult) -> String {
    let mut methods: Vec<&str> = interim
        .input_requests
        .iter()
        .flat_map(|requests| requests.values())
        .filter_map(|request| request.get("method").and_then(Value::as_str))
        .collect();
    methods.sort_unstable();
    methods.dedup();
    if methods.is_empty() {
        "the server asked to be retried".to_string()
    } else {
        methods.join(", ")
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
