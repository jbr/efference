//! `PATCH /sessions/:session`: a session's state (docs/control-api.md).
//!
//! The session resource has a location whether or not it is running, so reviving
//! one is a change to that resource rather than a verb on a path of its own.
//! [`SessionState`] is the lifecycle, kept distinct from the `status` a listing
//! reports: that is what a *live* session is doing (`running`, `idle`, `gated`,
//! `asking`, `stalled`), while this is whether it is here at all.
//!
//! `active` reopens a dormant session under its own id — [`Host::resume`], which
//! is the second boot entry behind it. `archived` is what `DELETE` already did, so
//! the two share one state word and one transition rather than describing one
//! thing two ways.
//!
//! `supervisor` is the other field a patch changes: which session supervises this
//! one (`supervise.rs`, docs/permissions.md). It is a separate patch from `state`.

use super::*;
use serde::{Deserialize, Serialize};

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
    /// This attempt's base environment, when the caller supplies one
    /// (docs/env.md). `None` asks the host to establish it for `cwd`, as a
    /// creation's is.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub env: Option<BTreeMap<String, String>>,
    /// Set this running session's supervisor to the session named, or clear it with
    /// `null` (docs/permissions.md). Absent leaves it alone. Not combined with
    /// `state` in one patch.
    #[serde(
        default,
        deserialize_with = "present",
        skip_serializing_if = "Option::is_none"
    )]
    pub supervisor: Option<Option<String>>,
}

/// Read a field whose `null` means something: absent is `None` (by `default`), and a
/// present value — `null` included — is `Some`.
fn present<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> std::result::Result<Option<Option<String>>, D::Error> {
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
            env: None,
            supervisor: None,
        }
    }

    /// The patch that sets a running session's supervisor, or clears it with `None`.
    pub fn supervise(supervisor: Option<String>) -> Self {
        Self {
            state: None,
            cwd: None,
            reauth: Vec::new(),
            env: None,
            supervisor: Some(supervisor),
        }
    }
}

impl Host {
    /// Resume a dormant session under its own id: what `PATCH {"state":
    /// "active"}` does (docs/control-api.md).
    ///
    /// The three states of a name are live (attach), free (mint), and
    /// taken-but-dormant, and this is the last one — a session whose directory is
    /// still on disk with nothing hosting it. Its log is reopened rather than
    /// created and its own `config.toml` is the live config, so no option in
    /// `body` is read: the selections, effort and temperature come from the header
    /// the session was created with (docs/log-and-projection.md).
    ///
    /// That is why the patch that reaches here carries no options field to
    /// override them with — an omitted one that resolved from history would quietly
    /// move the session onto a different model than its log records. `cwd` does
    /// come from the caller, and not as a preference: the host serves several
    /// roots and cannot tell which one a dormant id belongs to.
    ///
    /// Returns `AlreadyActive` when this host is already running the session, so
    /// the route can answer a repeat patch with the state it asked for rather than
    /// an error — and `Conflict` when the session is live under a different root,
    /// which is a disagreement about which session this is rather than a repeat.
    pub(super) async fn resume(
        self: &Arc<Self>,
        id: String,
        body: PatchSession,
    ) -> std::result::Result<Resume, ResumeRefused> {
        if !crate::daemon::tcp::plain_id(&id) {
            return Err(ResumeRefused::Refused(format!(
                "session name {id:?} is not a single plain path segment"
            )));
        }
        let Some(cwd) = body.cwd else {
            return Err(ResumeRefused::Refused(
                "an `active` patch needs `cwd`: the root whose .efference holds this session"
                    .into(),
            ));
        };
        if !cwd.is_absolute() {
            return Err(ResumeRefused::Refused(format!(
                "cwd must be absolute: {}",
                cwd.display()
            )));
        }
        // A live session is not dormant. Reopening its log would be the second
        // writer the writer lock exists to prevent, so this answers with the state
        // the caller asked for instead — but only when the root agrees, since a
        // session hosted elsewhere is a different session that happens to share an
        // id, and quietly claiming it would attach a client to the wrong one.
        if let Some(hosted) = self.hosted(&id) {
            // The patch's `cwd` names the root holding the session's `.efference`: its
            // project, which differs from the working directory in a worktree.
            let hosted = hosted.session.directories();
            let hosted_cwd = hosted.project();
            return if hosted_cwd == cwd {
                Err(ResumeRefused::AlreadyActive)
            } else {
                Err(ResumeRefused::Conflict(format!(
                    "session {id:?} is hosted under {}, not {}",
                    hosted_cwd.display(),
                    cwd.display()
                )))
            };
        }
        let session_dir = cwd.join(SESSION_BASE).join(&id);
        if !session_dir.is_dir() {
            return Err(ResumeRefused::Refused(format!(
                "no session {id:?} in {}: nothing at {}",
                cwd.display(),
                session_dir.display()
            )));
        }
        // An environment the caller supplied is the base, and nothing is run. An
        // omitted one is established here, for this session's own directory, on
        // the same terms a creation's is (docs/env.md).
        let base_env = match body.env {
            Some(env) => match crate::env_table::EnvSnapshot::new(env) {
                Ok(base) => base,
                Err(e) => return Err(ResumeRefused::Refused(format!("env: {e}"))),
            },
            None => {
                let abort = self.swansong.shutting_down();
                match crate::env_table::environment_at(
                    &cwd,
                    self.inference.env_hook(),
                    &self.host_env,
                    abort,
                )
                .await
                {
                    Ok(base) => base,
                    Err(e) => return Err(ResumeRefused::Refused(format!("env: {e}"))),
                }
            }
        };
        // Creator and `detached` as the header recorded them.
        let (created_by, detached) = self.recorded_lineage(&session_dir);
        let (boot, options) = match crate::boot::resume(crate::boot::ResumeRequest {
            session_dir,
            base_env,
            inference: &self.inference,
            reauth: body.reauth,
            host: Some(Arc::clone(self)),
        })
        .await
        {
            Ok(boot) => boot,
            Err(e) => {
                return Err(ResumeRefused::Refused(format!(
                    "could not resume the session: {e}"
                )));
            }
        };
        let runtime = SessionRuntime::start(boot, self.swansong());
        self.adopt_with(id.clone(), runtime, created_by, detached)?;
        // The options `boot::resume` inferred from the header, recorded so a
        // `hand_off` from this session hands its successor the same place rather
        // than the host's defaults (docs/tools.md). A resume adopts, so this is
        // where its `Hosted.options` comes from.
        self.record_options(&id, options);
        log::info!("session {id}: resumed");
        Ok(Resume::Resumed)
    }
}

impl Host {
    /// The creator and `detached` flag the session's header recorded.
    ///
    /// The creator is kept only while this host still holds it. A `created_by`
    /// naming a session that is not here would end the resumed one when a session
    /// nobody is running goes away, so a dangling one is dropped and the session
    /// stands on its own. A header that cannot be read yields neither, and is
    /// left to `boot::resume`, which refuses with the real reason.
    pub(crate) fn recorded_lineage(&self, session_dir: &std::path::Path) -> (Option<String>, bool) {
        let Some(recording) =
            crate::persist::load_recording(session_dir.join(crate::boot::LOG_NAME)).ok()
        else {
            return (None, false);
        };
        let Some(metadata) = recording.header.metadata() else {
            return (None, false);
        };
        let created_by = metadata
            .created_by
            .clone()
            .filter(|creator| self.session(creator).is_some());
        (created_by, metadata.detached)
    }
}

/// What an `active` patch did.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Resume {
    /// The log was reopened and the session started.
    Resumed,
}

/// Why an `active` patch did not reopen anything, apart from the reason it
/// could not.
#[derive(Debug)]
pub(super) enum ResumeRefused {
    /// Already running here under the requested root: the state the caller asked
    /// for is the state it is in, so this answers the same as a success.
    AlreadyActive,
    /// Running under a different root. A `409`, not a success — the caller is
    /// describing a session this host is not serving.
    Conflict(String),
    /// The reason it could not be done, as a `400`.
    Refused(String),
}

/// A reason from the host's own refusals is a `400` — the shape every other
/// adoption path reports one in.
impl From<String> for ResumeRefused {
    fn from(reason: String) -> Self {
        Self::Refused(reason)
    }
}

/// `PATCH /sessions/:session`: change one session's state. Mounted on every host
/// listener beside `DELETE /sessions/:session`, and registered ahead of the
/// `/sessions/:session/*` wildcard so the session is not resolved into conn state
/// first — a patch that *makes* a session live must not require it to be.
pub(super) async fn patch_session(mut conn: Conn) -> Conn {
    let Some(host) = host_of(&conn) else {
        return missing_host(conn);
    };
    let id = conn.param("session").unwrap_or_default().to_string();
    let body = conn.request_body_string().await.unwrap_or_default();
    let body: PatchSession = match serde_json::from_str(&body) {
        Ok(body) => body,
        Err(error) => {
            return fail(
                conn,
                ErrorCode::BadRequest,
                format!("invalid body: {error}"),
            );
        }
    };
    let state = match (body.state, body.supervisor.clone()) {
        (Some(_), Some(_)) => {
            return fail(
                conn,
                ErrorCode::BadRequest,
                "`state` and `supervisor` are separate patches",
            );
        }
        (None, Some(supervisor)) => return supervise(conn, &host, id, supervisor),
        (None, None) => {
            return fail(
                conn,
                ErrorCode::BadRequest,
                "nothing to patch: `state` and `supervisor` are the fields this route changes",
            );
        }
        (Some(state), None) => state,
    };
    match state {
        SessionState::Active => match host.resume(id.clone(), body).await {
            // Both outcomes are the state the caller asked for, so both answer
            // 200; `resumed` is what tells a repeat apart from the change.
            Ok(Resume::Resumed) => active(conn, id, true),
            Err(ResumeRefused::AlreadyActive) => active(conn, id, false),
            Err(ResumeRefused::Conflict(reason)) => fail(conn, ErrorCode::StateMismatch, reason),
            Err(ResumeRefused::Refused(reason)) => fail(conn, ErrorCode::BadRequest, reason),
        },
        // Archiving is what `DELETE` does, spelled as the state change it is. It
        // reads the session the host is already running, so the 404 is the
        // not-hosted one rather than a bad request.
        SessionState::Archived => match host.end(&id) {
            Some(completion) => {
                let retired = completion.await;
                let mut body = serde_json::json!({
                    "id": id, "state": state, "resumed": false,
                });
                if let Some(worktree) = retired.and_then(|retired| retired.response()) {
                    body["worktree"] = worktree;
                }
                conn.with_status(200).with_body(body.to_string()).halt()
            }
            None => no_such_session(conn, &id),
        },
    }
}

/// A `supervisor` patch (docs/permissions.md): `200 {"id", "state": "active",
/// "supervisor"}` with the pointer as it now reads, `404` for a session that is not
/// hosted, `400` naming why the change was refused.
fn supervise(conn: Conn, host: &Host, id: String, supervisor: Option<String>) -> Conn {
    match host.set_supervisor(&id, supervisor) {
        Ok(supervisor) => conn
            .with_status(200)
            .with_body(
                serde_json::json!({
                    "id": id, "state": SessionState::Active, "supervisor": supervisor,
                })
                .to_string(),
            )
            .halt(),
        Err(SupervisorRefused::NotHosted(_)) => no_such_session(conn, &id),
        Err(SupervisorRefused::Refused(reason)) => fail(conn, ErrorCode::BadRequest, reason),
    }
}

/// The `200` an `active` patch answers with, saying whether this call is what
/// made the session live.
fn active(conn: Conn, id: String, resumed: bool) -> Conn {
    conn.with_status(200)
        .with_body(
            serde_json::json!({
                "id": id, "state": SessionState::Active, "resumed": resumed,
            })
            .to_string(),
        )
        .halt()
}
