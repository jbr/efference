//! The session-control tools: one definition, two entry points (docs/operating.md).
//!
//! Each of the twelve verbs that drive a *running* session is one control-API call
//! over the session's socket (docs/control-channel.md), written once here as an
//! [mcplease](mcplease) tool. The `tools!` macro below turns them into a single
//! `Tools` enum that is simultaneously
//!
//! - a clap `Subcommand`, mounted as `efference control <tool>`, and
//! - the `tools/list` + `tools/call` dispatch, served over stdin/stdout by
//!   `efference mcp` so another harness can supervise a session by tool call.
//!
//! `mcplease::run` is not used: it owns argv parsing and logger init, and efference
//! has both already.
//!
//! Two module names dodge a dependency of the same name (`use image::Image` would be
//! ambiguous against the `image` crate): the `image` and `log` tools live in
//! `read_image.rs` and `read_log.rs`. The advertised tool names are unaffected.
//!
//! `Tool::execute` is synchronous and [`Client`] is async, so [`Control`] carries a
//! [`Handle`] to the runtime `main` is already on and each tool blocks on its one
//! call. Both entry points therefore run dispatch on a blocking thread
//! (`tokio::task::spawn_blocking`), never on a runtime worker.

use std::future::Future;
use std::time::Duration;

use anyhow::{Result, anyhow};
use efference::client::Client;
use mcplease::traits::ToolOutput;
use mcplease::types::ContentBlock;
use serde::Serialize;
use serde_json::Value;
use tokio::runtime::Handle;

use super::{CliResult, resolve_client_socket};

mcplease::tools!(
    Control,
    (Turn, turn, "turn"),
    (Approve, approve, "approve"),
    (Settle, settle, "settle"),
    (Interrupt, interrupt, "interrupt"),
    (Background, background, "background"),
    (Pace, pace, "pace"),
    (ReviewLevel, review_level, "review_level"),
    (Notify, notify, "notify"),
    (Dismiss, dismiss, "dismiss"),
    (Image, read_image, "image"),
    (State, state, "state"),
    (Log, read_log, "log")
);

/// What a mounting harness is told about this server, verbatim from
/// docs/operating.md: what the tools drive, what a gate is, and which call to
/// reach for at each of the three things that can be waited on.
const INSTRUCTIONS: &str = "\
Drive one running efference session (a daemon started with `efference serve`). The \
session runs its own agent loop; these tools submit work to it, resolve the gates it \
stops at, and read where it is.

A *gate* is the loop pausing on a tool call the permission rungs did not clear: the \
call is held, nothing runs, and the session waits for a decision. `state` reports the \
pending gate; `approve` resolves it (approve, `abort` to unwind the whole turn, or \
`redirect` to reject this call and steer with a message).

Waiting: `turn` with `wait`, `approve` with `wait`, and `settle` all block until the \
session reaches its next decision point — a gate, or the turn completing — and return \
its `/state` then. Pass `timeout_s`; on expiry they return the current state with \
`settled: false` rather than an error.

`turn` interrupts what the session is doing with a new user message; `notify` files an \
ambient message that waits for the next natural break instead. Prefer `notify` for \
anything that is not urgent.";

/// The tool state: which session the tools drive, and how a synchronous
/// [`Tool`](mcplease::traits::Tool) reaches an async client.
///
/// Session selection is deliberately two knobs and no more — the socket resolved at
/// startup from `--name`/`--socket`/discovery, and an optional per-call `session`
/// override. Who *may* drive which session is a separate design; nothing here
/// pre-empts it.
pub(crate) struct Control {
    /// The runtime `main` is on. Every tool blocks on one client call through it.
    runtime: Handle,
    /// The session resolved at startup, used by any call that names no `session`.
    default_client: Client,
    /// Whether this dispatch came from the command line rather than from an MCP
    /// client. Only `image` reads it: at a terminal it writes a file and says so,
    /// over MCP it hands back the bytes as an image content block.
    cli: bool,
}

impl Control {
    /// Bind the tools to the session behind `socket`, blocking on `runtime`.
    pub(crate) fn new(runtime: Handle, socket: std::path::PathBuf, cli: bool) -> Self {
        Self {
            runtime,
            default_client: Client::new(socket),
            cli,
        }
    }

    /// Bind the tools to an already-built client — the seam a test drives the real
    /// control router through on an in-memory transport.
    #[cfg(test)]
    pub(crate) fn with_client(runtime: Handle, default_client: Client, cli: bool) -> Self {
        Self {
            runtime,
            default_client,
            cli,
        }
    }

    /// The client for one call: the `session` override if given, else the session
    /// resolved at startup. An override names a session the same way `--name` does.
    fn client(&self, session: Option<&str>) -> Result<Client> {
        match session {
            None => Ok(self.default_client.clone()),
            Some(name) => Ok(Client::new(
                resolve_client_socket(Some(name), None).map_err(|e| anyhow!("{e}"))?,
            )),
        }
    }

    /// Whether this dispatch came from the command line. See [`Control::cli`].
    fn is_cli(&self) -> bool {
        self.cli
    }

    /// Block the calling thread on one client call.
    fn block_on<F: Future>(&self, future: F) -> F::Output {
        self.runtime.block_on(future)
    }

    /// Await a wait that answers with a `/state` body, bounded by `timeout_s`.
    ///
    /// An expired wait is not an error: MCP clients time tool calls out, and a caller
    /// that has to tell "the session is still working" from "the socket broke" has been
    /// given the wrong answer. So on expiry this reads the unwaited `/state` and marks
    /// it `settled: false`.
    fn settle_within<F>(
        &self,
        client: &Client,
        timeout_s: Option<u64>,
        wait: F,
    ) -> Result<StateResult>
    where
        F: Future<Output = Result<String>>,
    {
        self.block_on(async {
            let Some(seconds) = timeout_s else {
                return StateResult::settled(&wait.await?);
            };
            match tokio::time::timeout(Duration::from_secs(seconds), wait).await {
                Ok(body) => StateResult::settled(&body?),
                Err(_) => StateResult::unsettled(&client.state().await?),
            }
        })
    }
}

/// A `/state` snapshot as a tool result (docs/control-api.md).
#[derive(Serialize, schemars::JsonSchema)]
pub(crate) struct StateResult {
    /// Present only on a call that waited. `false` means the wait hit its
    /// `timeout_s` and this is where the session was at that moment, not where it
    /// stopped.
    #[serde(skip_serializing_if = "Option::is_none")]
    settled: Option<bool>,
    /// The daemon's ephemeral control state: the pending gate if there is one, the
    /// loop's status, the inbox, and the running processes.
    state: Value,
}

impl StateResult {
    /// A snapshot nobody waited for — no `settled` field to report.
    fn plain(body: &str) -> Result<Self> {
        Ok(Self {
            settled: None,
            state: serde_json::from_str(body)?,
        })
    }

    fn settled(body: &str) -> Result<Self> {
        Ok(Self {
            settled: Some(true),
            state: serde_json::from_str(body)?,
        })
    }

    fn unsettled(body: &str) -> Result<Self> {
        Ok(Self {
            settled: Some(false),
            state: serde_json::from_str(body)?,
        })
    }
}

/// A `GET /log` read as a tool result (docs/control-api.md).
#[derive(Serialize, schemars::JsonSchema)]
pub(crate) struct LogResult {
    /// The durable append-only log window: the entries from `since` onward.
    log: Value,
}

impl LogResult {
    fn new(body: &str) -> Result<Self> {
        Ok(Self {
            log: serde_json::from_str(body)?,
        })
    }
}

// `structured_output!`'s expansion, plus a `to_text` that pretty-prints: the
// program-facing half and the model-facing half stay the compact JSON the macro
// would give, and only the command-line rendering is unfolded, which is what the
// verbs these replaced printed.
impl ToolOutput for StateResult {
    fn output_schema() -> Option<Value> {
        Some(mcplease::traits::schema_for::<Self>().into())
    }

    fn to_text(&self) -> String {
        pretty(self)
    }
}

impl ToolOutput for LogResult {
    fn output_schema() -> Option<Value> {
        Some(mcplease::traits::schema_for::<Self>().into())
    }

    fn to_text(&self) -> String {
        pretty(self)
    }
}

/// Pretty-printed JSON for the command-line path, falling back to the compact form
/// if it somehow will not render.
fn pretty<T: Serialize>(value: &T) -> String {
    serde_json::to_string_pretty(value)
        .or_else(|_| serde_json::to_string(value))
        .unwrap_or_default()
}

/// One logged result image, handed back the way the caller can use it: as bytes on
/// disk at a terminal, as an image content block over MCP (docs/control-api.md).
#[derive(Serialize)]
#[serde(untagged)]
pub(crate) enum ImageResult {
    /// Written to a file — the command-line path, where there is nothing to render a
    /// content block.
    Wrote { path: String, bytes: usize },
    /// The bytes themselves, base64'd, for an MCP client to render.
    Inline {
        #[serde(skip)]
        data: String,
        mime_type: String,
        bytes: usize,
    },
}

impl ImageResult {
    /// The MCP shape: the bytes base64'd, with the media type read off the bytes
    /// themselves — an image block a client can render.
    fn inline(bytes: &[u8], format: Option<image::ImageFormat>) -> Self {
        use base64::Engine as _;
        Self::Inline {
            data: base64::engine::general_purpose::STANDARD.encode(bytes),
            mime_type: format
                .map(|f| f.to_mime_type().to_string())
                .unwrap_or_else(|| "application/octet-stream".into()),
            bytes: bytes.len(),
        }
    }
}

impl ToolOutput for ImageResult {
    fn to_content(&self) -> Vec<ContentBlock> {
        match self {
            Self::Wrote { path, bytes } => {
                vec![ContentBlock::text(format!("wrote {path} ({bytes} bytes)"))]
            }
            Self::Inline {
                data, mime_type, ..
            } => vec![ContentBlock::Image {
                data: data.clone(),
                mime_type: mime_type.clone(),
                annotations: None,
                meta: None,
            }],
        }
    }

    fn structured_content(&self) -> Option<Value> {
        None
    }
}

/// The daemon's one pending approval, read out of `GET /state` — what a bare
/// `approve` with no call id resolves. The loop is sequential, so there is at most
/// one (docs/README.md).
pub(crate) async fn pending_call_id(client: &Client) -> Result<String> {
    let state: Value = serde_json::from_str(&client.state().await?)?;
    state
        .get("gate")
        .and_then(|gate| gate.get("call_id"))
        .and_then(|id| id.as_str())
        .map(str::to_string)
        .ok_or_else(|| anyhow!("no pending approval; pass a call id explicitly"))
}

/// Run `efference control <tool>` and print what it returned. Dispatch is
/// synchronous, so it runs on a blocking thread rather than on a runtime worker.
pub(crate) async fn run_cli(tools: Tools, socket: std::path::PathBuf) -> CliResult {
    let runtime = Handle::current();
    let text = tokio::task::spawn_blocking(move || {
        let mut state = Control::new(runtime, socket, true);
        mcplease::traits::Dispatch::call_to_text(
            tools,
            &mut state,
            &mcplease::types::RequestContext::default(),
        )
    })
    .await??;
    println!("{text}");
    Ok(())
}

/// Run `efference mcp`: the same tools over newline-delimited JSON-RPC on
/// stdin/stdout, the transport a harness mounting this server speaks
/// (docs/operating.md). Blocks until stdin closes.
pub(crate) async fn run_stdio(socket: std::path::PathBuf) -> CliResult {
    let runtime = Handle::current();
    tokio::task::spawn_blocking(move || {
        let mut state = Control::new(runtime, socket, false);
        let config =
            mcplease::ServerConfig::new(mcplease::server_info!()).with_instructions(INSTRUCTIONS);
        mcplease::serve::<Tools, _>(&mut state, &config)
    })
    .await??;
    Ok(())
}

#[cfg(test)]
mod test;
