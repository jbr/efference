//! The session-control tools: one definition, two entry points (docs/operating.md).
//!
//! Each verb that drives a *running* session is one control-API call
//! over its control channel (docs/control-channel.md), and the three that
//! address the host — `sessions`, `create`, `end` — are one host-route call each.
//! Every one is written once here as an [mcplease](mcplease) tool. The `tools!` macro below turns them into a single
//! `Tools` enum that is simultaneously
//!
//! - a clap `Subcommand`, mounted as `efference control <tool>`, and
//! - the `tools/list` + `tools/call` dispatch, served over stdin/stdout by
//!   `efference mcp` so another harness can supervise a session by tool call.
//!
//! `mcplease::run` is not used: it owns argv parsing and logger init, and efference
//! has both already.
//!
//! One module name dodges a dependency of the same name (`use image::Image` would be
//! ambiguous against the `image` crate): the `image` tool lives in `read_image.rs`.
//! The advertised tool name is unaffected.
//!
//! `Tool::execute` is synchronous and the client executor is async, so the
//! [`Executor::Client`] variant carries the [`Handle`] to the runtime `main` is
//! already on and each tool blocks on its one call. Both entry points therefore run
//! dispatch on a blocking thread (`tokio::task::spawn_blocking`), never on a runtime
//! worker.

use std::future::Future;
use std::path::PathBuf;
use std::time::Duration;

use crate::client::Client;
use anyhow::{Result, anyhow};
use mcplease::traits::ToolOutput;
use mcplease::types::ContentBlock;
use serde::Serialize;
use serde_json::Value;
use tokio::runtime::Handle;

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
    (Transcript, transcript, "transcript"),
    (Entries, entries, "entries"),
    (Title, title, "title"),
    (Sessions, sessions, "sessions"),
    (Create, create, "create"),
    (End, end, "end")
);

/// What a mounting harness is told about this server, verbatim from
/// docs/operating.md: what the tools drive, what a gate is, and which call to
/// reach for at each of the three things that can be waited on.
pub const INSTRUCTIONS: &str = "\
Drive the efference sessions on one host. Each session runs its own agent loop; \
these tools submit work to it, resolve the gates it stops at, and read where it is. \
`sessions` lists what the host holds, `create` starts a session in a working \
directory, `end` tears one down. Every other tool takes a `session` id; it is \
required unless this server was started against one session, and then it \
overrides that session for the call.

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
anything that is not urgent.

Reading: `transcript` is what the session has said and done, one line per entry with \
its `seq` — prose in full, calls by name and intent, results by size and verdict, no \
bodies. `entries` fetches whole entries by seq when a line is not enough; `image` \
fetches an image a result carried.";

/// The tool state: an executor and the presentation requested by its mount.
pub struct Control {
    executor: Executor,
    /// Whether this dispatch came from the command line rather than from an MCP
    /// client. Only `image` reads it: at a terminal it writes a file and says so,
    /// over MCP it hands back the bytes as an image content block.
    cli: bool,
}

impl Control {
    /// Bind tools to an executor. `cli` selects terminal image output rather than
    /// MCP image blocks.
    pub fn new(executor: Executor, cli: bool) -> Self {
        Self { executor, cli }
    }

    /// Bind the tools to an already-built client — the seam a test drives the real
    /// control router through on an in-memory transport. The host is a path
    /// nothing listens on.
    #[cfg(test)]
    fn with_client(runtime: Handle, default_client: Client, cli: bool) -> Self {
        Self::new(
            Executor::client(
                runtime,
                PathBuf::from(".efference/host.sock"),
                Fallback::Client(default_client),
            ),
            cli,
        )
    }

    /// Whether this dispatch came from the command line. See [`Control::cli`].
    fn is_cli(&self) -> bool {
        self.cli
    }
}

/// How control tools reach a session. The CLI and stdio MCP server both use the
/// HTTP client today. A future in-process mount adds its own variant; each tool
/// explicitly selects its executor in its `execute` implementation.
pub enum Executor {
    Client(ClientExecutor),
}

impl Executor {
    /// Bind the HTTP executor to a host socket and fallback session selection.
    pub fn client(runtime: Handle, host: PathBuf, fallback: Fallback) -> Self {
        Self::Client(ClientExecutor {
            runtime,
            host,
            fallback,
        })
    }
}

/// The HTTP-over-control-channel executor used by the CLI and MCP mounts.
pub struct ClientExecutor {
    runtime: Handle,
    host: PathBuf,
    fallback: Fallback,
}

/// Which session a client executor drives when a call names no `session`.
#[allow(clippy::large_enum_variant)]
pub enum Fallback {
    /// One already reached: a resolved `--name`/`--socket`, or a test's in-memory
    /// transport.
    Client(Client),
    /// Resolve from `--name`/`--socket`/discovery on first use, then keep it. The
    /// command line's default, so `efference control state` in a directory with
    /// one session needs no name and a host-only verb never triggers discovery.
    Resolve {
        name: Option<String>,
        socket: Option<PathBuf>,
    },
    /// Nothing: the server was mounted against the host, and every call names its
    /// session. `efference mcp`'s default.
    None,
}

impl ClientExecutor {
    /// The client for one call: the `session` id if given — a session the host
    /// lists, or one with its own live socket under `.efference/` — else the
    /// fallback. Without a fallback, a call that names no session is an error that
    /// says what the host holds.
    fn client(&mut self, session: Option<&str>) -> Result<Client> {
        if let Some(name) = session {
            return self.resolve(Some(name), None);
        }
        match &self.fallback {
            Fallback::Client(client) => Ok(client.clone()),
            Fallback::Resolve { name, socket } => {
                let (name, socket) = (name.clone(), socket.clone());
                let client = self.resolve(name.as_deref(), socket)?;
                self.fallback = Fallback::Client(client.clone());
                Ok(client)
            }
            Fallback::None => {
                let held = match self.host_client() {
                    Ok(host) => match self.block_on(host.list_sessions()) {
                        Ok(ids) if ids.is_empty() => {
                            "the host holds no sessions; `create` one".to_string()
                        }
                        Ok(ids) => format!("the host holds: {}", ids.join(", ")),
                        Err(e) => format!("the host could not be listed: {e:#}"),
                    },
                    Err(e) => e.to_string(),
                };
                Err(anyhow!(
                    "pass `session`: this server is mounted against the host, not one session ({held})"
                ))
            }
        }
    }

    fn resolve(&self, name: Option<&str>, socket: Option<PathBuf>) -> Result<Client> {
        if let Some(socket) = socket {
            return Ok(Client::new(socket));
        }
        let hosted = if Self::is_live(&self.host) {
            self.block_on(Client::new(&self.host).list_sessions())?
        } else {
            Vec::new()
        };
        if let Some(name) = name {
            let own = PathBuf::from(crate::daemon::SESSION_BASE)
                .join(name)
                .join(crate::daemon::SOCKET_NAME);
            if Self::is_live(&own) {
                return Ok(Client::new(own));
            }
            if hosted.iter().any(|id| id == name) {
                return Ok(Client::at(&self.host, &format!("/sessions/{name}")));
            }
            return Err(anyhow!(
                "no live session {name:?}: nothing listens at {} and the host{} does not list it",
                own.display(),
                if Self::is_live(&self.host) {
                    String::new()
                } else {
                    format!(" at {} is not running", self.host.display())
                }
            ));
        }
        let mut live: Vec<(String, Client)> = Self::discover_live_sockets()?
            .into_iter()
            .map(|socket| (socket.display().to_string(), Client::new(socket)))
            .collect();
        live.extend(hosted.iter().map(|id| {
            (
                format!("{} (/sessions/{id})", self.host.display()),
                Client::at(&self.host, &format!("/sessions/{id}")),
            )
        }));
        match live.len() {
            0 => Err(anyhow!(
                "no live efference session found — create one with `tui --new`, or pass --name/--socket"
            )),
            1 => Ok(live.pop().unwrap().1),
            _ => Err(anyhow!(
                "multiple live sessions ({}); pass --name to choose",
                live.into_iter()
                    .map(|(description, _)| description)
                    .collect::<Vec<_>>()
                    .join(", ")
            )),
        }
    }

    fn is_live(socket: &std::path::Path) -> bool {
        std::os::unix::net::UnixStream::connect(socket).is_ok()
    }

    fn discover_live_sockets() -> Result<Vec<PathBuf>> {
        let mut sockets = Vec::new();
        let base = std::path::Path::new(crate::daemon::SESSION_BASE);
        if base.is_dir() {
            for entry in std::fs::read_dir(base)? {
                let socket = entry?.path().join(crate::daemon::SOCKET_NAME);
                if Self::is_live(&socket) {
                    sockets.push(socket);
                }
            }
        }
        sockets.sort();
        Ok(sockets)
    }

    /// A client at the host's socket root, for the listener's own routes. Checked
    /// live first, so a host that is not running reads as that rather than as a
    /// connection error inside a request.
    fn host_client(&self) -> Result<Client> {
        if !Self::is_live(&self.host) {
            return Err(anyhow!(
                "no host at {}; start one with `efference host`",
                self.host.display()
            ));
        }
        Ok(Client::new(&self.host))
    }

    /// Block the calling thread on one client call.
    fn block_on<F: Future>(&self, future: F) -> F::Output {
        self.runtime.block_on(future)
    }

    /// Await a wait that answers with a `/state` body, bounded by `timeout_s`.
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
pub struct StateResult {
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

/// Whole log entries fetched by seq (`GET /log/:seq`, docs/control-api.md).
#[derive(Serialize, schemars::JsonSchema)]
pub struct EntriesResult {
    /// One full entry view per requested seq, in the order asked.
    entries: Vec<Value>,
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

impl ToolOutput for EntriesResult {
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
pub enum ImageResult {
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

#[cfg(test)]
mod test;
