//! Resuming a dormant session (docs/control-api.md, docs/resumption.md): `active` in a
//! `PATCH /sessions/:session` reopens it under its own id — [`Host::resume`], which is the
//! second boot entry behind it. The route is `host_http/patch.rs`.

use super::preparation::authorization_for;
use super::*;
use crate::host::starting::{Starting, StartingParts};
use crate::protocol::state::Stage;
use crate::record::session_dir::{SESSION_BASE, plain_id};

impl Host {
    /// Resume a dormant session under its own id: what `PATCH {"state":
    /// "active"}` does (docs/control-api.md).
    ///
    /// Its log is reopened rather than created and its own `config.toml` is the live
    /// config, so no option in `body` is read except `reauth`,
    /// `refuse_authorization`, `env` and the spotter and triage `switch`: the main
    /// model, effort and temperature come from the header the session was created
    /// with, and a patch carrying them could quietly move the session onto a
    /// different model than its log records. `cwd` comes from the caller, and not
    /// as a preference: the host serves several roots and cannot tell which one a
    /// dormant id belongs to.
    ///
    /// Returns once the session is in the map as starting, like a creation; the
    /// environment and `boot::resume` run in the background (`preparation.rs`).
    /// Returns `AlreadyActive` when this host is already running or starting the
    /// session, so the route can answer a repeat patch with the state it asked for
    /// rather than an error — and `Conflict` when the session is live under a
    /// different root, which is a disagreement about which session this is rather
    /// than a repeat.
    pub(crate) fn resume(
        self: &Arc<Self>,
        id: &str,
        body: PatchSession,
    ) -> Result<Resume, ResumeRefused> {
        if !plain_id(id) {
            return Err(ResumeRefused::Refused(format!(
                "session name {id:?} is not a single plain path segment"
            )));
        }
        let Some(cwd) = body.cwd.clone() else {
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
        // id, and quietly claiming it would attach a client to the wrong one. The
        // patch's `cwd` names the root holding the session's `.efference`: its
        // project, which differs from the working directory in a worktree.
        if let Some(entry) = self.entry(id) {
            let held = match &entry {
                Entry::Hosted(hosted) => hosted.session.directories().project().to_path_buf(),
                Entry::Starting(starting) => starting.project().to_path_buf(),
            };
            return if held == cwd {
                Err(ResumeRefused::AlreadyActive)
            } else {
                Err(ResumeRefused::Conflict(format!(
                    "session {id:?} is hosted under {}, not {}",
                    held.display(),
                    cwd.display()
                )))
            };
        }
        let session_dir = cwd.join(SESSION_BASE).join(id);
        // The register holds the id under one root. A directory of the same name
        // under another is a different session sharing the name, which the host
        // cannot key (docs/session-title.md); a root never registered claims it.
        if session_dir.is_dir()
            && let Err(held) = self.session_ids.claim(id, &cwd)
        {
            return Err(ResumeRefused::Conflict(format!(
                "session id {id:?} is registered under {}, not {}",
                held.root.display(),
                cwd.display()
            )));
        }
        if !session_dir.is_dir() {
            return Err(ResumeRefused::Refused(format!(
                "no session {id:?} in {}: nothing at {}",
                cwd.display(),
                session_dir.display()
            )));
        }
        // An environment the caller supplied is the base, and nothing is run.
        let supplied_env = match body.env.clone() {
            Some(env) => Some(
                crate::config::env_table::EnvSnapshot::new(env)
                    .map_err(|e| ResumeRefused::Refused(format!("env: {e}")))?,
            ),
            None => None,
        };
        // Creator, `detached`, and where the session works, as the header recorded them.
        let recorded = self.recorded_lineage(&session_dir);
        let starting = Starting::new(StartingParts {
            id: id.to_string(),
            directories: crate::config::policy::Directories::new(
                cwd.clone(),
                recorded.working.clone().unwrap_or_else(|| cwd.clone()),
            ),
            resume: true,
            created_by: recorded.created_by.clone(),
            detached: recorded.detached,
        });
        if !self.sessions.lock().unwrap().insert_starting(&starting) {
            return Err(ResumeRefused::AlreadyActive);
        }
        self.touch_listing();
        log::info!("session {id}: resuming");
        let host = Arc::clone(self);
        let preparing = Arc::clone(&starting);
        self.prepare_in_background(&starting, async move {
            host.prepare_resume(&preparing, session_dir, body, supplied_env, recorded)
                .await
        });
        Ok(Resume::Starting)
    }

    /// The rest of a resume, once `starting` holds its id: establish the
    /// environment, reopen, start, and adopt.
    async fn prepare_resume(
        self: &Arc<Self>,
        starting: &Arc<Starting>,
        session_dir: PathBuf,
        body: PatchSession,
        supplied_env: Option<crate::config::env_table::EnvSnapshot>,
        recorded: RecordedLineage,
    ) -> Result<(), String> {
        let id = starting.id().to_string();
        // An omitted environment is established for this session's own directory, on
        // the same terms a creation's is (docs/env.md).
        let base_env = match supplied_env {
            Some(base) => base,
            None => {
                self.establish_environment(starting, starting.project())
                    .await?
            }
        };
        starting.set_stage(Stage::Preparing);
        let (boot, options) = boot::resume(boot::ResumeRequest {
            session_dir,
            base_env,
            inference: &self.inference,
            reauth: body.reauth,
            authorization: authorization_for(starting, body.refuse_authorization),
            switch: body.switch,
            host: Some(Arc::clone(self)),
        })
        .await
        .map_err(|e| format!("could not resume the session: {e}"))?;
        let runtime = SessionRuntime::start(boot, self.swansong());
        let session = Arc::clone(runtime.session());
        self.adopt_prepared(
            id.clone(),
            runtime,
            recorded.created_by,
            recorded.detached,
            Some(starting),
        )?;
        // The options `boot::resume` inferred from the header, recorded so a
        // `hand_off` from this session hands its successor the same place rather
        // than the host's defaults (docs/tools.md). A resume adopts, so this is
        // where its `Hosted.options` comes from.
        self.record_options(&id, options);
        self.clear_resume_failure(starting.project(), &id);
        starting.started(session);
        log::info!("session {id}: resumed");
        Ok(())
    }
}

/// What a dormant session's header recorded about where it sits.
pub(crate) struct RecordedLineage {
    /// The creator, kept only while this host holds it.
    pub(crate) created_by: Option<String>,
    pub(crate) detached: bool,
    /// The working directory, when it is not the project (docs/worktrees.md).
    pub(crate) working: Option<PathBuf>,
}

impl Host {
    /// The creator, `detached` flag, and working directory the session's header
    /// recorded.
    ///
    /// The creator is kept only while this host still holds it. A `created_by`
    /// naming a session that is not here would end the resumed one when a session
    /// nobody is running goes away, so a dangling one is dropped and the session
    /// stands on its own. A header that cannot be read yields none of them, and is
    /// left to `boot::resume`, which refuses with the real reason.
    pub(crate) fn recorded_lineage(&self, session_dir: &std::path::Path) -> RecordedLineage {
        let unread = RecordedLineage {
            created_by: None,
            detached: false,
            working: None,
        };
        let Some(recording) = crate::record::persist::load_recording(
            session_dir.join(crate::record::session_dir::LOG_NAME),
        )
        .ok() else {
            return unread;
        };
        let Some(metadata) = recording.header.metadata() else {
            return unread;
        };
        let created_by = metadata
            .created_by
            .clone()
            .filter(|creator| self.session(creator).is_some());
        RecordedLineage {
            created_by,
            detached: metadata.detached,
            working: metadata.working.clone(),
        }
    }
}

/// What an `active` patch did.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Resume {
    /// The session is starting: its log will be reopened once the environment and
    /// its MCP servers are prepared.
    Starting,
}

/// Why an `active` patch did not reopen anything, apart from the reason it
/// could not.
#[derive(Debug)]
pub(crate) enum ResumeRefused {
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
