//! The session-control tools: one definition, two entry points (docs/operating.md).
//!
//! Each verb that drives a *running* session is one control-API call
//! over its control channel (docs/control-channel.md), and the four that
//! address the host — `sessions`, `create`, `archive`, `supervisor` — are one
//! host-route call each.
//! Every one is written once here as an [mcplease](mcplease) tool. The `tools!` macro below turns them into a single
//! `Tools` enum that is simultaneously
//!
//! - a clap `Subcommand`, mounted as `efference control <tool>`, and
//! - the `tools/list` + `tools/call` dispatch, served over stdin/stdout by
//!   `efference mcp` so another harness can supervise a session by tool call.
//!
//! The binary drives `mcplease::calls` itself for the second: argv parsing, the
//! logger, and the runtime are already efference's.
//!
//! One module name dodges a dependency of the same name (`use image::Image` would be
//! ambiguous against the `image` crate): the `image` tool lives in `read_image.rs`.
//! The advertised tool name is unaffected.
//!
//! `Tool::execute` is async, so each tool awaits its one call on whatever runtime
//! drives it: the binary's for the command line and `efference mcp`, the host's
//! for a session's own toolbox.

use std::future::Future;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::client::{Client, Waited};
use crate::log::ToolCall;
use anyhow::{Result, anyhow};

use mcplease::traits::ToolOutput;
use mcplease::types::ContentBlock;
use serde::{Deserialize, Serialize};
use serde_json::Value;

mcplease::tools!(
    Control,
    (Turn, turn, "turn"),
    (Approve, approve, "approve"),
    (Settle, settle, "settle"),
    (Interrupt, interrupt, "interrupt"),
    (Background, background, "background"),
    (Pace, pace, "pace"),
    (ReviewLevel, review_level, "review_level"),
    (Supervisor, supervisor, "supervisor"),
    (Notify, notify, "notify"),
    (Proposal, proposal, "proposal"),
    (Image, read_image, "image"),
    (State, state, "state"),
    (Transcript, transcript, "transcript"),
    (Entries, entries, "entries"),
    (Title, title, "title"),
    (Sessions, sessions, "sessions"),
    (Create, create, "create"),
    (HandOff, hand_off, "hand_off"),
    (Archive, archive, "archive"),
    (Subscribe, subscribe, "subscribe"),
    (Unsubscribe, unsubscribe, "unsubscribe")
);

/// What a mounting harness is told about this server, verbatim from
/// docs/operating.md: what the tools drive, what a gate is, and which call to
/// reach for at each of the three things that can be waited on.
pub const INSTRUCTIONS: &str = "\
Drive the efference sessions on one host. Each session runs its own agent loop; \
these tools submit work to it, resolve the gates it stops at, and read where it is. \
`sessions` lists what the host holds, `create` starts a session in a working \
directory, `archive` stops one and keeps its log. Every other tool takes a `session` id; it is \
required unless this server was started against one session, and then it \
overrides that session for the call.

A *gate* is the loop pausing on a tool call the permission rungs did not clear: the \
call is held, nothing runs, and the session waits for a decision. `state` reports the \
pending gate; `approve` resolves it (approve, `abort` to unwind the whole turn, or \
`redirect` to reject this call and steer with a message).

Waiting: `turn` with `wait`, `approve` with `wait`, and `settle` all block until the \
session reaches its next decision point — a gate, a question, the turn completing, or a \
stop that waits on someone (a cold park, a block) — and return its `/state` then, whose \
`loop_phase` names which. Pass `timeout_s`; on expiry they return the current state with \
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

/// `hand_off`'s wire name in a session's toolbox: the call that gives the
/// session's place to a new one, which is always put to the operator
/// (docs/permissions.md).
pub const HAND_OFF_WIRE_NAME: &str = "sessions__hand_off";

/// `archive`'s wire name in a session's toolbox. A call to it naming the caller's own
/// predecessor is the hand-off's completion and clears on the fast path
/// (docs/permissions.md).
pub const ARCHIVE_WIRE_NAME: &str = "sessions__archive";

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
    /// The leaf session's working directory — what its relative paths resolve
    /// against.
    pub cwd: PathBuf,
    /// The leaf session's project directory, which its grants resolve against;
    /// with `cwd`, the directories `policy::classify` runs with.
    pub project: PathBuf,
    /// The session the parent's call directly names — the top of `chain`.
    pub session: String,
    /// The session ids from that target down to the session holding the leaf,
    /// inclusive. One entry for a direct approval; longer when the held call is
    /// itself an approval. Only participating approvers appear.
    pub chain: Vec<String>,
}

impl BoundAction {
    /// The leaf session's project and working directories, for classification.
    pub fn directories(&self) -> crate::policy::Directories {
        crate::policy::Directories::new(self.project.clone(), self.cwd.clone())
    }
}

/// The tool state: an executor and the presentation requested by its mount.
/// Cloned into every call, which may run alongside others.
#[derive(Clone)]
pub struct Control {
    executor: Executor,
    /// Whether this dispatch came from the command line rather than from an MCP
    /// client. `image` reads it — at a terminal it writes a file and says so, over MCP
    /// it hands back the bytes as an image content block — and `transcript`, which
    /// reads the audit grain at a terminal and the conversation over MCP
    /// (docs/control-api.md).
    cli: bool,
}

impl Control {
    /// Bind tools to an executor. `cli` selects the command line's presentation: terminal
    /// image output rather than MCP image blocks, and the transcript's audit grain.
    pub fn new(executor: Executor, cli: bool) -> Self {
        Self { executor, cli }
    }

    /// Bind the tools to an already-built client — the interface a test drives the real
    /// control router through on an in-memory transport. The host is a path
    /// nothing listens on.
    #[cfg(test)]
    fn with_client(default_client: Client, cli: bool) -> Self {
        Self::new(
            Executor::client(
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
    /// Whether `call` is this session archiving its own predecessor — the session whose
    /// recorded succession names the caller (docs/permissions.md). Only the in-process
    /// [`Executor::Host`] knows the caller; the host wrote the link at `hand_off`, so the
    /// agent cannot assert it.
    pub(crate) fn completes_hand_off(&self, call: &ToolCall) -> bool {
        match &self.executor {
            Executor::Host(state) => state.completes_hand_off(call),
            Executor::Client(_) => false,
        }
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
#[derive(Clone)]
pub enum Executor {
    Client(ClientExecutor),
    Host(HostExecutor),
}

impl Executor {
    /// Bind the HTTP executor to a host socket and fallback session selection.
    pub fn client(host: PathBuf, fallback: Fallback) -> Self {
        Self::Client(ClientExecutor {
            host,
            fallback: Arc::new(Mutex::new(fallback)),
        })
    }

    /// Bind the in-process executor: the calling session driving its project's
    /// sessions through the host it already lives in (docs/tools.md). The host
    /// is held weakly — the host owns every session and a session's toolbox
    /// holds this executor, so a strong reference would be a cycle.
    pub fn host(host: Arc<crate::daemon::Host>, caller: String) -> Self {
        Self::Host(HostExecutor {
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
/// 2. a mutating verb reaches only the caller's descendants (via `created_by`) and
///    the sessions it supervises (via the host's supervisor pointer), so a session
///    cannot resolve its own gate or steer a peer it was not given.
#[derive(Clone)]
pub struct HostExecutor {
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
    /// caller must be an ancestor of the target or its supervisor.
    fn resolve_mutating(&self, requested: Option<&str>) -> Result<Arc<crate::daemon::Session>> {
        let target = self.target(requested)?;
        let session = self.in_project(&target)?;
        let host = self.host()?;
        if !Self::may_drive(&host, &self.caller, &target) {
            return Err(anyhow!(
                "refused: {:?} neither created nor supervises {target:?} — a mutating call \
                 reaches only the caller's descendants and the sessions it supervises \
                 (docs/tools.md)",
                self.caller
            ));
        }
        Ok(session)
    }

    /// Whether `call` is an `archive` of the caller's predecessor, by the host's
    /// succession record. False when the host holds no record — after a host restart the
    /// link is not restored, so nothing clears here then.
    fn completes_hand_off(&self, call: &ToolCall) -> bool {
        if call.name != ARCHIVE_WIRE_NAME {
            return false;
        }
        let Some(target) = call.input.get("session").and_then(Value::as_str) else {
            return false;
        };
        let Ok(host) = self.host() else {
            return false;
        };
        host.predecessor(&self.caller).as_deref() == Some(target)
    }

    /// The target for `archive`: a mutating verb's rules, with one more session in
    /// reach — the caller's own predecessor, which handed its work to the caller
    /// and is the caller's to archive once it has no more questions (docs/tools.md).
    fn resolve_archive(&self, requested: Option<&str>) -> Result<Arc<crate::daemon::Session>> {
        let target = self.target(requested)?;
        if self.host()?.predecessor(&self.caller).as_deref() == Some(target.as_str()) {
            return self.in_project(&target);
        }
        self.resolve_mutating(Some(&target))
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

    /// Rule 2: whether `driver` may use a mutating verb on `target` — `target` is
    /// `driver`'s descendant, or `driver` is `target`'s supervisor (docs/tools.md).
    /// Supervision is not transitive: it reaches the supervised session, not the
    /// sessions that session created or supervises.
    fn may_drive(host: &crate::daemon::Host, driver: &str, target: &str) -> bool {
        Self::chain_reaches(host, driver, target)
            || host.supervisor_of(target).as_deref() == Some(driver)
    }

    /// Bind a `sessions__approve` call to the leaf action its target holds
    /// (docs/permissions.md). The first hop obeys the same target rules the Host
    /// arm enforces when it runs — never self, descendants only, same project.
    /// While the held call is itself an approval, follow its `session`+`call_id`
    /// through the host to the leaf, each hop required to be a descendant of the
    /// previous or supervised by it; refuse on a moved gate or a cycle.
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
                    cwd: gate.directories.working().to_path_buf(),
                    project: gate.directories.project().to_path_buf(),
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
            if !Self::may_drive(&host, &previous, &next) {
                return Err(anyhow!(
                    "refused: {previous:?} neither created nor supervises {next:?} — a \
                     mutating call reaches only the caller's descendants and the sessions it \
                     supervises (docs/tools.md)"
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

    /// `create` in-session: the child records the caller as its creator
    /// (docs/tools.md).
    async fn create(&self, body: crate::daemon::CreateSession) -> Result<String> {
        let host = self.host()?;
        let lineage = crate::env::Lineage {
            created_by: Some(self.caller.clone()),
            succeeds: None,
        };
        // Waits out the preparation, so the calling agent reads a failure as this
        // call's error rather than finding the child gone later.
        let starting = host
            .create(body, lineage)
            .map_err(|reason| anyhow!("{reason}"))?;
        starting
            .finished()
            .await
            .map_err(|reason| anyhow!("{reason}"))?;
        Ok(starting.id().to_string())
    }

    /// `hand_off` in-session: the caller's work goes to a new session in its own
    /// directory, which it then succeeds (docs/tools.md).
    async fn hand_off(&self, body: crate::daemon::CreateSession, brief: String) -> Result<String> {
        let host = self.host()?;
        let handed = host.hand_off(&self.caller, body, brief).await;
        handed.map_err(|reason| anyhow!("{reason}"))
    }

    /// `subscribe` in-session: the caller hears the target's status
    /// (docs/tools.md). `Ok(false)` when the target is no longer hosted.
    fn subscribe(&self, target: &str, wake: bool) -> Result<bool> {
        Ok(self.host()?.subscribe(target, &self.caller, wake))
    }

    /// `unsubscribe` in-session. `Ok(false)` when there was no subscription.
    fn unsubscribe(&self, target: &str) -> Result<bool> {
        Ok(self.host()?.unsubscribe(target, &self.caller))
    }

    /// `archive` in-session: wait for the target's teardown.
    async fn archive(&self, target: &str) -> Result<()> {
        if let Some(done) = self.host()?.end(target) {
            done.await;
        }
        Ok(())
    }

    /// The caller's project: every session sharing its storage base.
    fn sessions(&self) -> Result<Vec<crate::client::ListedSession>> {
        let host = self.host()?;
        let caller = self.caller_session()?;
        let base = caller.storage_base();
        Ok(host
            .listing()
            .into_iter()
            .filter(|row| {
                host.session(&row.id)
                    .is_some_and(|session| session.storage_base() == base)
            })
            .collect())
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
#[derive(Clone)]
pub struct ClientExecutor {
    host: PathBuf,
    /// Shared by every clone, so a fallback resolved by one call is kept for the
    /// rest.
    fallback: Arc<Mutex<Fallback>>,
}

/// Which session a client executor drives when a call names no `session`.
#[allow(clippy::large_enum_variant)]
#[derive(Clone)]
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
    async fn client(&self, session: Option<&str>) -> Result<Client> {
        if let Some(name) = session {
            return self.resolve(Some(name), None).await;
        }
        // Cloned out rather than held: resolving awaits, and the lock is sync.
        // Two calls racing to resolve both reach the same answer.
        let fallback = self
            .fallback
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        match fallback {
            Fallback::Client(client) => Ok(client),
            Fallback::Resolve { name, socket } => {
                let client = self.resolve(name.as_deref(), socket).await?;
                *self.fallback.lock().unwrap_or_else(|e| e.into_inner()) =
                    Fallback::Client(client.clone());
                Ok(client)
            }
            Fallback::None => {
                let held = match self.host_client() {
                    Ok(host) => match host.list_sessions().await {
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

    async fn resolve(&self, name: Option<&str>, socket: Option<PathBuf>) -> Result<Client> {
        if let Some(socket) = socket {
            return Ok(Client::new(socket));
        }
        // The host's listing, scoped to this directory: one host serves many
        // projects, and a verb run here means a session here
        // (docs/control-channel.md). A listing without a cwd is kept.
        let hosted: Vec<String> = if Self::is_live(&self.host) {
            let here = std::env::current_dir()?;
            Client::new(&self.host)
                .sessions()
                .await?
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

    /// Run a waiting call and answer with its `/state`, bounded by `timeout_s` measured from
    /// the start of the call. `first` is the acting request (a turn, an approval, or a bare
    /// settle). The host caps each wait and says so (docs/control-api.md), so a capped
    /// answer is followed by fresh settle polls until one is settled or the deadline passes;
    /// an omitted `timeout_s` has no deadline (docs/operating.md).
    async fn settle_within<F>(
        &self,
        client: &Client,
        timeout_s: Option<u64>,
        first: F,
    ) -> Result<StateResult>
    where
        F: Future<Output = Result<Waited>>,
    {
        let wait = async {
            let mut answer = first.await?;
            while !answer.settled {
                answer = client.settle().await?;
            }
            StateResult::settled(&answer.body)
        };
        let Some(seconds) = timeout_s else {
            return wait.await;
        };
        match tokio::time::timeout(Duration::from_secs(seconds), wait).await {
            Ok(result) => result,
            Err(_) => StateResult::unsettled(&client.state().await?),
        }
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
    /// The daemon's ephemeral control state: where the loop is (`loop_phase`), the
    /// pending gate if there is one, the pending question (`ask`) if the loop is waiting
    /// on a reply, the inbox, and the running processes.
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
