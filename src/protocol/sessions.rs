//! The host's session collection (docs/control-api.md): the bodies that create, preview
//! and patch a session, and the rows that list the live, archived and dormant ones.
//!
//! Each body is one type on both ends of the channel — the control client serializes
//! it and the host deserializes it — so a new option is one field here. Reading the
//! rows' files and acting on the bodies is the host's (`daemon::host`).

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::inference::{ResumeSwitch, SessionOptions};
use crate::inventory::InventoryRow;

/// Body of `POST /sessions` (docs/control-api.md): a `SessionRequest` as a
/// client spells it. One type on both ends — the control client serializes it,
/// the host deserializes it — so a new session knob is one field here. `env` is
/// the session's complete base environment (docs/env.md); it is applied and
/// dropped, never logged or echoed, which is why the type has no Debug.
///
/// `env` is optional because an omitted one is a *different request* from an
/// empty one. Supplied, it is the base and nothing runs; omitted, the base is
/// established for the session's directory from the user-wide `env_hook`, or
/// from the host's own environment when none is configured (docs/env.md). An
/// empty map is an explicit empty environment, so `{}` and omitted are different
/// requests.
#[derive(Serialize, Deserialize)]
// The control channel's JSON body: each flag is its own optional key, and a client sends
// any subset of them.
#[allow(clippy::struct_excessive_bools)]
pub struct CreateSession {
    /// The session id; a minted adjective-noun pair when absent
    /// (docs/session-title.md). A single plain path segment, as the gate's
    /// `plain_id` requires.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// The directory the session works in. Absolute, always sent. The session's
    /// project is this directory, or the project whose worktree it is inside
    /// (docs/worktrees.md).
    pub cwd: PathBuf,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config: Option<PathBuf>,
    /// The editable selections, spelled exactly as `QUERY /sessions` accepts
    /// them and resolved by the same flow (docs/control-api.md). Flattened, so
    /// the two routes cannot drift and a new option is one field in one place.
    #[serde(flatten)]
    pub options: SessionOptions,
    #[serde(default)]
    pub upstream: Vec<String>,
    #[serde(default)]
    pub autonomous: bool,
    #[serde(default)]
    pub reauth: Vec<String>,
    /// Fail the mount of an MCP server that asks for authorization, naming why, instead
    /// of starting the authorization dance on the host (docs/mcp-auth.md): for a
    /// preparation nobody may be at the host to finish.
    #[serde(default)]
    pub refuse_authorization: bool,
    #[serde(default)]
    pub keep_scratch: bool,
    /// Give the session its own git worktree and start it there
    /// (docs/worktrees.md).
    #[serde(default)]
    pub worktree: bool,
    /// Outlive the session that creates this one (docs/tools.md). Meaningful only
    /// for a session-driven `create`: a human root has no creator to end with.
    #[serde(default)]
    pub detached: bool,
    /// The session's complete base environment, when the caller supplies one
    /// (docs/env.md). `None` asks the host to establish it for `cwd`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub env: Option<BTreeMap<String, String>>,
}

impl CreateSession {
    /// A creation in `cwd` with every other field at its default: a minted name, the
    /// probed config, the host's defaults for every role, the environment established
    /// by the host.
    pub fn new(cwd: PathBuf) -> Self {
        Self {
            name: None,
            cwd,
            config: None,
            options: SessionOptions::default(),
            upstream: Vec::new(),
            autonomous: false,
            reauth: Vec::new(),
            refuse_authorization: false,
            keep_scratch: false,
            worktree: false,
            detached: false,
            env: None,
        }
    }
}

/// The body of `QUERY /sessions` (docs/control-api.md): where the proposed
/// session would run, plus the same editable selections creation accepts.
#[derive(Serialize, Deserialize)]
pub struct SessionOptionsRequest {
    /// The proposed session's project root. Absolute, as creation's is: it is
    /// what a relative `config` resolves against.
    pub cwd: PathBuf,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config: Option<PathBuf>,
    #[serde(flatten)]
    pub options: SessionOptions,
}

impl SessionOptionsRequest {
    pub fn new(cwd: PathBuf) -> Self {
        Self {
            cwd,
            config: None,
            options: SessionOptions::default(),
        }
    }
}

/// Whether a session is running on this host.
///
/// Deliberately two states. A deliberate archive and a host that went away are
/// different histories, and the inventory asks to keep them apart; they are the
/// same thing to act on, and an operator resuming a session does not care which
/// it was.
#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "lowercase")]
pub enum SessionState {
    /// Running here. Patching this on a session that is not running reopens it
    /// under its own id.
    Active,
    /// Stopped, with its directory and log intact. Resumable, by patching
    /// `active` again.
    Archived,
}

/// A partial update to a session: the fields present are the ones being changed.
///
/// One type for every field a patch can carry, so a new one is a field here and
/// nothing else. `state` is the only one that describes the session; the rest
/// supply what the transition needs.
#[derive(Serialize, Deserialize)]
pub struct PatchSession {
    /// What to make the session's lifecycle be. Omitted leaves it alone — a patch
    /// that changed nothing is refused rather than read as agreement.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<SessionState>,
    /// The working root whose `.efference` holds this session. Absolute.
    ///
    /// Required to make a session `active`, because a host serving several roots
    /// cannot tell which one a dormant id belongs to; not read by an `archived`
    /// patch, which acts on a session this host is already running.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<PathBuf>,
    /// MCP servers to re-authorize, discarding stored credentials first. Only read
    /// by an `active` patch, and not a property of the session: the same request
    /// that reopens a session says what to bring up with it.
    #[serde(default)]
    pub reauth: Vec<String>,
    /// Fail the mount of an MCP server that asks for authorization instead of
    /// starting the dance on the host (docs/mcp-auth.md). Only read by an `active`
    /// patch: a resume nobody may be at the host to finish.
    #[serde(default)]
    pub refuse_authorization: bool,
    /// This attempt's base environment, when the caller supplies one
    /// (docs/env.md). `None` asks the host to establish it for `cwd`, as a
    /// creation's is.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub env: Option<BTreeMap<String, String>>,
    /// Spotter and triage selections to resume on, in the shapes creation takes
    /// (docs/resumption.md). Only read by an `active` patch; a field left out keeps
    /// the one the session recorded.
    #[serde(flatten)]
    pub switch: ResumeSwitch,
    /// Set this running session's supervisor to the session named, or clear it with
    /// `null` (docs/permissions.md). Absent leaves it alone. Not combined with
    /// `state` in one patch.
    #[serde(
        default,
        deserialize_with = "present",
        skip_serializing_if = "Option::is_none"
    )]
    // Absent leaves the supervisor alone and `null` clears it: the outer `Option` is the
    // key's presence in the control channel's JSON.
    #[allow(clippy::option_option)]
    pub supervisor: Option<Option<String>>,
    /// Who is asking, spelled as a turn's `source` is (`web`, `mcp[<name>]`,
    /// `session[<id>]`); recorded on the `Archived` entry of an `archived` patch and
    /// read by nothing else (docs/log-and-projection.md).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
}

/// Read a field whose `null` means something: absent is `None` (by `default`), and a
/// present value — `null` included — is `Some`.
// The control channel's JSON tells an absent `supervisor` (leave it alone) from `null`
// (clear it), and `deserialize_with` fixes this return type to the field's.
#[allow(clippy::option_option)]
fn present<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<Option<String>>, D::Error> {
    Option::<String>::deserialize(deserializer).map(Some)
}

impl PatchSession {
    /// The patch that makes the session under `cwd`'s `.efference` active, with
    /// the host establishing its environment.
    pub fn activate(cwd: PathBuf) -> Self {
        Self {
            state: Some(SessionState::Active),
            cwd: Some(cwd),
            reauth: Vec::new(),
            refuse_authorization: false,
            env: None,
            switch: ResumeSwitch::default(),
            supervisor: None,
            source: None,
        }
    }

    /// The patch that sets a running session's supervisor, or clears it with `None`.
    pub fn supervise(supervisor: Option<String>) -> Self {
        Self {
            state: None,
            cwd: None,
            reauth: Vec::new(),
            refuse_authorization: false,
            env: None,
            switch: ResumeSwitch::default(),
            supervisor: Some(supervisor),
            source: None,
        }
    }
}

/// One row of `GET /sessions` (docs/control-api.md). Also the shape the
/// `sessions` tool answers with, hence the schema derive (docs/tools.md).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, schemars::JsonSchema)]
pub struct ListedSession {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// The session's project directory, so a client can keep to its own
    /// project's sessions on a host that serves several
    /// (docs/control-channel.md).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    /// The session's working directory, when it is not the project: a session in
    /// a git worktree (docs/worktrees.md).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub working: Option<String>,
    /// The session that commissioned this one, when a session did
    /// (docs/tools.md). Absent for a human root.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_by: Option<String>,
    /// What the session is doing, as a single word: `starting` while it is
    /// prepared, then `running`, `idle`, `gated`, `asking`, `stalled`, `parked`, or
    /// `blocked`.
    pub status: String,
    /// Live background processes the session holds (docs/process-monitor.md). Ending
    /// it reaps them, which is why a listing carries the count.
    #[serde(default)]
    pub processes: usize,
}

/// One archived session as `GET /sessions?state=archived` lists it: the
/// inventory's row, with `state` saying which collection it is from.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct ArchivedSession {
    #[serde(flatten)]
    pub row: InventoryRow,
    pub state: SessionState,
    /// Why this host's last attempt to resume the session failed, until one
    /// succeeds (the host's preparation). Unlike the row's `problem`, the session is still
    /// resumable: this is what the last try met, not what the directory is.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resume_failure: Option<String>,
}

/// One dormant session as `GET /sessions/:session` answers it: the listing's row,
/// and what a client offering the model switches at resume reads
/// (docs/resumption.md). A field the session's files cannot answer is left out.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct DormantSession {
    #[serde(flatten)]
    pub archived: ArchivedSession,
    /// The main, spotter and triage selections the session last ran on, in the
    /// shapes an `active` patch takes, read as a resume reads them
    /// ([`crate::boot::recorded_selections`]). Absent when the log cannot be read or
    /// records none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selections: Option<ResumeSwitch>,
    /// The prompt tokens of the session's last main-loop request: what reading the
    /// context uncached would cost, the number the `cold` topic reports as
    /// `context_tokens` for a hosted session. Absent when the usage record holds no
    /// `turn` line.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_tokens: Option<u64>,
}
