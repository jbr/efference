//! A starting session's preparation on its own task, and the ways it ends without a
//! session: a failure, an archive, and the host's shutdown (docs/control-api.md,
//! docs/resumption.md).
//!
//! Creation and resume each reserve the id as a starting entry and hand their
//! preparation here. A failure takes the entry out of the map: a creation leaves no
//! directory (`boot::prepare` rolls back what it minted) and gives its id back, and a
//! resume leaves the session dormant with the reason as its `resume_failure`. The entry
//! itself stays readable for [`FAILURE_KEPT`], so a client that attaches after the
//! failure still reads it. An archive cancels the preparation the same way a host
//! shutdown does, and leaves nothing a creation minted.

use super::*;
use crate::boot::mcp_mount::AuthorizationHandling;
use crate::daemon::starting::Starting;
use crate::session_dir::SESSION_BASE;
use std::time::Instant;

/// How long a failed starting session keeps answering `/state` and `/events` after it
/// left the map: room for a client that created it, and is still on its way to the
/// session view, to read why.
const FAILURE_KEPT: Duration = Duration::from_secs(300);

impl Host {
    /// Run `work`, a starting session's preparation, on its own task. An archive
    /// ([`Starting::cancel`]) or the host's shutdown drops it mid-await, which is what
    /// rolls back what a creation minted; a guard on the host's `Swansong` holds the
    /// shutdown open until that has run.
    pub(super) fn prepare_in_background(
        self: &Arc<Self>,
        starting: &Arc<Starting>,
        work: impl Future<Output = Result<(), String>> + Send + 'static,
    ) {
        let host = Arc::clone(self);
        let starting = Arc::clone(starting);
        let guard = self.swansong.guard();
        let shutting_down = self.swansong.shutting_down();
        tokio::spawn(async move {
            // Biased toward the work: a preparation that has finished is not then
            // treated as cancelled.
            let finished = tokio::select! {
                biased;
                result = work => Some(result),
                () = starting.cancelled() => None,
                () = shutting_down => None,
            };
            // A work error after a cancel is the cancel reaching it first (the env
            // hook's abort), not a failure of its own.
            let stopped = starting.is_cancelled() || !host.swansong.state().is_running();
            match finished {
                Some(Ok(())) => {}
                Some(Err(reason)) if !stopped => host.preparation_failed(&starting, reason),
                Some(Err(_)) | None => host.preparation_cancelled(&starting),
            }
            drop(guard);
        });
    }

    /// The env hook for a starting session, cancelled with it (docs/env.md).
    pub(super) async fn establish_environment(
        &self,
        starting: &Starting,
        root: &std::path::Path,
    ) -> Result<crate::env_table::EnvSnapshot, String> {
        let shutting_down = self.swansong.shutting_down();
        let abort = async move {
            tokio::select! {
                () = starting.cancelled() => {}
                () = shutting_down => {}
            }
        };
        crate::env_table::environment_at(root, self.inference.env_hook(), &self.host_env, abort)
            .await
            .map_err(|e| format!("env: {e}"))
    }

    /// Preparation failed: the entry leaves the map, a creation gives its id back, a
    /// resume records why on the dormant session, and the entry records the failure
    /// last, which ends its streams and wakes whoever waits on it to a host that
    /// already reflects it.
    fn preparation_failed(&self, starting: &Arc<Starting>, reason: String) {
        let id = starting.id().to_string();
        log::warn!("session {id}: did not start: {reason}");
        if starting.resume() {
            self.resume_failures.lock().unwrap().insert(
                (starting.project().to_path_buf(), id.clone()),
                reason.clone(),
            );
        }
        // Kept before the entry leaves the map, so a client resolving the id as it
        // changes finds one or the other.
        {
            let mut failed = self.failed.lock().unwrap();
            failed.retain(|_, (_, at)| at.elapsed() < FAILURE_KEPT);
            failed.insert(id, (Arc::clone(starting), Instant::now()));
        }
        self.leave_map(starting);
        starting.failed(reason);
    }

    /// Preparation was cancelled: archived while starting, or the host is shutting
    /// down. A creation leaves nothing; a resume archived on purpose ends its log in
    /// `Archived`, as any archive does (docs/resumption.md).
    fn preparation_cancelled(&self, starting: &Arc<Starting>) {
        let id = starting.id();
        let archiving = self.swansong.state().is_running();
        if starting.resume() && archiving {
            self.resume_failures
                .lock()
                .unwrap()
                .remove(&(starting.project().to_path_buf(), id.to_string()));
            let dir = starting.project().join(SESSION_BASE).join(id);
            if let Err(e) = append_archived(&dir) {
                log::warn!("session {id}: could not log the archive: {e}");
            }
        }
        self.leave_map(starting);
        if archiving {
            log::info!("session {id}: archived while starting");
            starting.cancelled_by("archived");
        } else {
            starting.cancelled_by("the host shut down");
        }
    }

    /// Take `starting` out of the map; a creation gives its id back.
    fn leave_map(&self, starting: &Arc<Starting>) {
        if self.sessions.lock().unwrap().remove_starting(starting) && !starting.resume() {
            self.session_ids.release(starting.id(), starting.project());
        }
        self.touch_listing();
    }

    /// A starting session whose preparation failed within [`FAILURE_KEPT`].
    pub(super) fn recently_failed(&self, id: &str) -> Option<Arc<Starting>> {
        let failed = self.failed.lock().unwrap();
        let (starting, at) = failed.get(id)?;
        (at.elapsed() < FAILURE_KEPT).then(|| Arc::clone(starting))
    }

    /// Why the last resume of the dormant session `id` under `root` failed.
    pub(super) fn resume_failure(&self, root: &std::path::Path, id: &str) -> Option<String> {
        self.resume_failures
            .lock()
            .unwrap()
            .get(&(root.to_path_buf(), id.to_string()))
            .cloned()
    }

    /// A resume started: forget why the last one failed.
    pub(super) fn clear_resume_failure(&self, root: &std::path::Path, id: &str) {
        self.resume_failures
            .lock()
            .unwrap()
            .remove(&(root.to_path_buf(), id.to_string()));
    }

    /// Archive a starting session: cancel its preparation and wait for what that
    /// leaves. One that started before the cancel landed is archived as a hosted
    /// session is.
    pub(super) async fn archive_starting(
        self: Arc<Self>,
        starting: Arc<Starting>,
        source: Option<String>,
    ) -> Ended {
        starting.cancel();
        if starting.finished().await.is_ok()
            && let Some(Entry::Hosted(hosted)) = self.entry(starting.id())
        {
            return self
                .end_hosted(hosted, starting.id().to_string(), source)
                .await;
        }
        Ended {
            retired: None,
            discarded: !starting.resume(),
        }
    }
}

/// What an MCP server asking for authorization meets while `starting` is prepared:
/// the dance names the server in the session's state while it waits, unless `refuse`
/// fails the mount instead (docs/mcp-auth.md).
pub(super) fn authorization_for(starting: &Arc<Starting>, refuse: bool) -> AuthorizationHandling {
    let starting = Arc::downgrade(starting);
    AuthorizationHandling {
        refuse,
        waiting: Some(Arc::new(move |server: Option<&str>| {
            if let Some(starting) = starting.upgrade() {
                starting.set_authorizing(server);
            }
        })),
    }
}

/// End a dormant session's log in `Archived`, unless its last lifecycle entry already
/// is one (docs/resumption.md).
fn append_archived(session_dir: &std::path::Path) -> io::Result<()> {
    let mut opened = Log::open(session_dir.join(crate::session_dir::LOG_NAME))
        .map_err(|e| io::Error::other(e.to_string()))?;
    let log = &mut opened.log;
    let last_lifecycle = log.entries().iter().rev().find(|entry| {
        matches!(
            entry.payload,
            Payload::Archived { .. } | Payload::Resumed(_)
        )
    });
    if matches!(
        last_lifecycle.map(|entry| &entry.payload),
        Some(Payload::Archived { .. })
    ) {
        return Ok(());
    }
    let turn = log.entries().last().map_or(0, |entry| entry.turn);
    log.append(turn, Payload::archived(None))?;
    Ok(())
}
