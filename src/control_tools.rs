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
use std::sync::Arc;
use std::time::Duration;

use crate::client::Client;
use crate::log::ToolCall;
use anyhow::{Result, anyhow};

use mcplease::traits::ToolOutput;
use mcplease::types::ContentBlock;
use serde::{Deserialize, Serialize};
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

/// The wire name of the in-session approval tool: the one call the loop binds to
/// its target's held action before running the permission ladder
/// (docs/permissions.md). The mount namespaces it `<server>__<tool>` for
/// `sessions` (docs/tools.md).
pub const APPROVE_WIRE_NAME: &str = "sessions__approve";

/// The leaf action a `sessions__approve` call authorizes (docs/permissions.md).
///
/// `sessions__approve` carries only a `call_id`, so the parent's ladder would
/// otherwise judge the approval rather than the move it authorizes. Before
/// classification the host binds the target's held call to the approval: the
/// leaf tool, its input as the child wrote it, its intent and cwd, and the chain
/// of sessions from the directly-targeted child down to the session holding the
/// leaf. The parent's fast-path, spotter, and human gate all read this instead
/// of the id.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct BoundAction {
    /// The leaf tool's wire name.
    pub name: String,
    /// The leaf call's input, as the child wrote it (its `intent` still inside,
    /// docs/tools.md).
    pub input: Value,
    /// The leaf call's one-line intent, lifted for the gate view.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub intent: Option<String>,
    /// The leaf's working directory — what its relative paths resolve against,
    /// and the cwd `policy::classify` runs with.
    pub cwd: PathBuf,
    /// The session the parent's call directly names — the top of `chain`.
    pub session: String,
    /// The session ids from that target down to the session holding the leaf,
    /// inclusive. One entry for a direct approval; longer when the held call is
    /// itself an approval. Only participating approvers appear.
    pub chain: Vec<String>,
}

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
    /// Resolve a `sessions__approve` call to the leaf action it authorizes
    /// (docs/permissions.md). Only the in-process [`Executor::Host`] can: it knows
    /// the caller and can read the target's pending gate. The HTTP executor never
    /// sees this call — the binding happens inside the target's own daemon.
    pub(crate) fn resolve_bound_action(&self, call: &ToolCall) -> Result<BoundAction> {
        match &self.executor {
            Executor::Host(state) => state.resolve_bound_action(call),
            Executor::Client(_) => Err(anyhow!(
                "no in-process host to resolve a bound action against (docs/tools.md)"
            )),
        }
    }
}

/// How control tools reach a session (docs/tools.md). The CLI and stdio MCP
/// server use the HTTP client; a session's own toolbox uses the in-process
/// `Host` executor and calls the host directly. Each tool explicitly selects
/// its executor in its `execute` implementation.
///
/// The client variant is much the larger (it carries the runtime handle, socket,
/// and fallback client); like [`Fallback`], the enum stays unboxed.
#[allow(clippy::large_enum_variant)]
pub enum Executor {
    Client(ClientExecutor),
    Host(HostExecutor),
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

    /// Bind the in-process executor: the calling session driving its project's
    /// sessions through the host it already lives in (docs/tools.md). The host
    /// is held weakly — the host owns every session and a session's toolbox
    /// holds this executor, so a strong reference would be a cycle.
    pub fn host(runtime: Handle, host: Arc<crate::daemon::Host>, caller: String) -> Self {
        Self::Host(HostExecutor {
            runtime,
            host: Arc::downgrade(&host),
            caller,
        })
    }
}

/// The in-process executor (docs/tools.md): the calling session's toolbox reaches
/// the host directly — the same `Session` methods the HTTP handlers call, never a
/// round trip over the socket. The two target rules live here because this is the
/// only executor that knows who is calling:
///
/// 1. the target is never the caller, and
/// 2. a mutating verb reaches only sessions in the caller's descendant chain
///    (via `created_by`), so a session cannot resolve its own gate or steer a peer.
pub struct HostExecutor {
    /// A handle to the host's runtime, for the two verbs that await real work
    /// (`create` boots a session; `end` waits for its teardown).
    runtime: Handle,
    /// The host, held weakly (see [`Executor::host`]).
    host: std::sync::Weak<crate::daemon::Host>,
    /// The calling session's id — the executor's identity.
    caller: String,
}

impl HostExecutor {
    /// The host, or an error if it has shut down.
    fn host(&self) -> Result<Arc<crate::daemon::Host>> {
        self.host
            .upgrade()
            .ok_or_else(|| anyhow!("the host is no longer running"))
    }

    /// The calling session itself, or an error if it has ended.
    fn caller_session(&self) -> Result<Arc<crate::daemon::Session>> {
        self.host()?
            .session(&self.caller)
            .ok_or_else(|| anyhow!("the calling session {:?} is no longer hosted", self.caller))
    }

    /// Resolve the target a call names, enforcing rule 1. A call that names no
    /// session defaults to the caller, which the rule then refuses — which is
    /// what makes an in-session read with no target an error rather than a
    /// silent self-read.
    fn target(&self, requested: Option<&str>) -> Result<String> {
        let target = requested.unwrap_or(&self.caller).to_string();
        if target == self.caller {
            return Err(anyhow!(
                "refused: {target:?} is the calling session — a session cannot target itself \
                 (docs/tools.md)"
            ));
        }
        Ok(target)
    }

    /// The target's `Session`, if it is in the caller's project — the sessions
    /// whose `.efference/` storage base is the caller's (docs/tools.md). A
    /// session outside it is not found, not forbidden: project scope is a
    /// visibility boundary, not a permission one.
    fn in_project(&self, target: &str) -> Result<Arc<crate::daemon::Session>> {
        let host = self.host()?;
        let caller = host
            .session(&self.caller)
            .ok_or_else(|| anyhow!("the calling session {:?} is no longer hosted", self.caller))?;
        let session = host.session(target).ok_or_else(|| {
            anyhow!("no session {target:?} in this project — `sessions` lists what it holds")
        })?;
        if session.storage_base() != caller.storage_base() {
            return Err(anyhow!(
                "no session {target:?} in this project — `sessions` lists what it holds"
            ));
        }
        Ok(session)
    }

    /// The target for a read or for `notify`: rule 1 and project scope.
    fn resolve(&self, requested: Option<&str>) -> Result<Arc<crate::daemon::Session>> {
        let target = self.target(requested)?;
        self.in_project(&target)
    }

    /// The target for a mutating verb: rule 1, project scope, and rule 2 — the
    /// caller must be an ancestor of the target.
    fn resolve_mutating(&self, requested: Option<&str>) -> Result<Arc<crate::daemon::Session>> {
        let target = self.target(requested)?;
        let session = self.in_project(&target)?;
        let host = self.host()?;
        if !self.is_ancestor(&host, &target) {
            return Err(anyhow!(
                "refused: {:?} is not an ancestor of {target:?} — a mutating call reaches only \
                 sessions the caller created (docs/tools.md)",
                self.caller
            ));
        }
        Ok(session)
    }

    /// Whether `target`'s `created_by` chain reaches `ancestor`. The tree is
    /// acyclic by construction; the `seen` set only guards a malformed one.
    fn chain_reaches(host: &crate::daemon::Host, ancestor: &str, target: &str) -> bool {
        let mut seen = std::collections::HashSet::new();
        let mut current = target.to_string();
        while seen.insert(current.clone()) {
            match host.created_by(&current) {
                Some(parent) if parent == ancestor => return true,
                Some(parent) => current = parent,
                None => return false,
            }
        }
        false
    }

    /// Whether `target`'s `created_by` chain reaches the caller.
    fn is_ancestor(&self, host: &crate::daemon::Host, target: &str) -> bool {
        Self::chain_reaches(host, &self.caller, target)
    }

    /// Bind a `sessions__approve` call to the leaf action its target holds
    /// (docs/permissions.md). The first hop obeys the same target rules the Host
    /// arm enforces when it runs — never self, descendants only, same project.
    /// While the held call is itself an approval, follow its `session`+`call_id`
    /// through the host to the leaf, each hop required to be a descendant of the
    /// previous; refuse on a moved gate or a cycle.
    fn resolve_bound_action(&self, call: &ToolCall) -> Result<BoundAction> {
        let requested = call.input.get("session").and_then(Value::as_str);
        // The parent's own call: rule 1, project scope, rule 2.
        let direct = self.resolve_mutating(requested)?;
        let direct_id = self.target(requested)?;
        let mut expected = call
            .input
            .get("call_id")
            .and_then(Value::as_str)
            .ok_or_else(|| {
                anyhow!(
                    "in-session `approve` requires `call_id`: read the target's `state` gate \
                     for the held call"
                )
            })?
            .to_string();
        let mut chain = vec![direct_id];
        let mut session = direct;
        let mut previous = chain[0].clone();
        let mut seen = std::collections::HashSet::new();
        seen.insert(previous.clone());
        loop {
            let gate = session.pending_gate().ok_or_else(|| {
                anyhow!(
                    "gate moved: no pending gate in {previous:?} — read its `state` for the \
                     held call"
                )
            })?;
            if gate.call.id.0 != expected {
                return Err(anyhow!(
                    "gate moved: {expected:?} is not the pending gate in {previous:?}"
                ));
            }
            if gate.call.name != APPROVE_WIRE_NAME {
                let intent = gate.call.intent().map(str::to_string);
                let top = chain[0].clone();
                return Ok(BoundAction {
                    name: gate.call.name,
                    input: gate.call.input,
                    intent,
                    cwd: gate.cwd,
                    session: top,
                    chain,
                });
            }
            // The held call is itself an approval: follow it to its own target.
            let host = self.host()?;
            let next = gate
                .call
                .input
                .get("session")
                .and_then(Value::as_str)
                .ok_or_else(|| anyhow!("the held `sessions__approve` names no session to follow"))?
                .to_string();
            if !Self::chain_reaches(&host, &previous, &next) {
                return Err(anyhow!(
                    "refused: {previous:?} is not an ancestor of {next:?} — a mutating call \
                     reaches only sessions the caller created (docs/tools.md)"
                ));
            }
            if !seen.insert(next.clone()) {
                return Err(anyhow!(
                    "refused: the approval chain revisits {next:?} — a cycle (docs/tools.md)"
                ));
            }
            session = self.in_project(&next)?;
            expected = gate
                .call
                .input
                .get("call_id")
                .and_then(Value::as_str)
                .ok_or_else(|| {
                    anyhow!("the held `sessions__approve` names no `call_id` to follow")
                })?
                .to_string();
            previous = next.clone();
            chain.push(next);
        }
    }

    /// Run one async host call to completion from the synchronous tool
    /// dispatch. The loop runs on the host's multi-thread runtime
    /// (docs/control-channel.md), so dispatch is on one of its workers:
    /// `block_in_place` hands that worker's queue to another thread and lets
    /// this one wait, and the future is driven by the rest of the runtime.
    /// Outside a multi-thread runtime — a current-thread test — the future
    /// runs on a scratch thread against the host runtime instead, since
    /// `block_in_place` would panic there.
    fn block_on<F, T>(&self, future: F) -> Result<T>
    where
        F: std::future::Future<Output = T> + Send + 'static,
        T: Send + 'static,
    {
        let runtime = self.runtime.clone();
        let multi_thread = tokio::runtime::Handle::try_current()
            .is_ok_and(|h| h.runtime_flavor() == tokio::runtime::RuntimeFlavor::MultiThread);
        if multi_thread {
            return Ok(tokio::task::block_in_place(|| runtime.block_on(future)));
        }
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(runtime.block_on(future));
        });
        rx.recv()
            .map_err(|_| anyhow!("the host call ended without a result"))
    }

    /// `create` in-session: the child records the caller as its creator
    /// (docs/tools.md).
    fn create(&self, body: crate::daemon::CreateSession) -> Result<String> {
        let host = self.host()?;
        let caller = self.caller.clone();
        let created = self.block_on(async move { host.create(body, Some(caller)).await })?;
        created.map_err(|reason| anyhow!("{reason}"))
    }

    /// `end` in-session: wait for the target's teardown.
    fn end(&self, target: &str) -> Result<()> {
        let host = self.host()?;
        let id = target.to_string();
        self.block_on(async move {
            if let Some(done) = host.end(&id) {
                done.await;
            }
        })
    }

    /// The caller's project: every session sharing its storage base.
    fn sessions(&self) -> Result<Vec<crate::client::SessionListing>> {
        let host = self.host()?;
        let caller = self.caller_session()?;
        let base = caller.storage_base();
        let mut list = Vec::new();
        for id in host.ids() {
            let Some(session) = host.session(&id) else {
                continue;
            };
            if session.storage_base() != base {
                continue;
            }
            let identity = session.identity();
            list.push(crate::client::SessionListing {
                title: identity
                    .as_ref()
                    .and_then(|identity| identity.title.clone()),
                cwd: identity.map(|identity| identity.cwd),
                created_by: host.created_by(&id),
                id,
            });
        }
        Ok(list)
    }
}

/// Whether two paths name the same directory: by canonical form when both
/// resolve, else as spelled (a session's identity carries the cwd its creator
/// sent, which is `current_dir` here when it is this directory).
fn same_dir(a: &std::path::Path, b: &std::path::Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => a == b,
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
        // The host's listing, scoped to this directory: one host serves many
        // projects, and a verb run here means a session here
        // (docs/control-channel.md). A listing without a cwd is kept.
        let hosted: Vec<String> = if Self::is_live(&self.host) {
            let here = std::env::current_dir()?;
            self.block_on(Client::new(&self.host).sessions())?
                .into_iter()
                .filter(|listing| {
                    listing
                        .cwd
                        .as_deref()
                        .is_none_or(|cwd| same_dir(std::path::Path::new(cwd), &here))
                })
                .map(|listing| listing.id)
                .collect()
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
