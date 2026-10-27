//! Archiving a session (docs/control-api.md, docs/resumption.md): [`Host::end`] shuts a
//! hosted session down and retires its own worktree, and a session that never had a turn
//! is discarded instead of archived.

use super::*;

/// What [`Host::end`] did: the outcome for the session's own worktree, and whether
/// the session was discarded instead of archived (docs/resumption.md).
#[derive(Debug)]
pub struct Ended {
    pub retired: Option<crate::worktree::Retired>,
    pub discarded: bool,
}

impl Ended {
    /// The `state` an archive answers with: `archived`, or `discarded` when the
    /// session was removed (docs/control-api.md).
    pub(super) fn state(&self) -> &'static str {
        if self.discarded {
            "discarded"
        } else {
            "archived"
        }
    }
}

impl Host {
    /// Archive one session: shut it down, then retire its own worktree when it made
    /// one (docs/control-api.md). Resolves when teardown has run, with what became of
    /// the worktree (`None` when the session had none, or its directory was already
    /// gone). `None` when no session has that id. The map entry is removed by the
    /// watcher [`adopt`](Self::adopt) spawned, once the loop has returned.
    ///
    /// This is the archive path only, which an island's exit also takes for each of
    /// its sessions before it shuts down (`island_exit.rs`). A host shutdown stops
    /// sessions without it and leaves every worktree in place, including through the
    /// cascade that ends a stopped session's children: those reach here with the host
    /// already shutting down, and retire nothing.
    ///
    /// A session still starting has its preparation cancelled instead
    /// (`preparation.rs`), leaving nothing a creation minted and a resumed session as
    /// dormant as it was.
    pub fn end(
        self: &Arc<Self>,
        id: &str,
    ) -> Option<Pin<Box<dyn std::future::Future<Output = Ended> + Send>>> {
        self.end_by(id, None)
    }

    /// [`end`](Self::end) for a caller that names itself: `source` is recorded on the
    /// session's `Archived` entry, spelled as a turn's `source` is (docs/log-and-projection.md).
    /// A session cancelled while it is still starting records none.
    pub fn end_by(
        self: &Arc<Self>,
        id: &str,
        source: Option<String>,
    ) -> Option<Pin<Box<dyn std::future::Future<Output = Ended> + Send>>> {
        Some(match self.entry(id)? {
            Entry::Hosted(hosted) => Box::pin(self.end_hosted(hosted, id.to_string(), source)),
            Entry::Starting(starting) => {
                Box::pin(Arc::clone(self).archive_starting(starting, source))
            }
        })
    }

    /// [`end`](Self::end) for a hosted session.
    pub(super) fn end_hosted(
        &self,
        hosted: Hosted,
        id: String,
        archived_by: Option<String>,
    ) -> impl std::future::Future<Output = Ended> + use<> {
        let archiving = self.swansong.state().is_running();
        let ids = self.session_ids.clone();
        async move {
            hosted.swansong.shut_down().await;
            let retired = if archiving {
                retire_worktree(&hosted.session, &id).await
            } else {
                None
            };
            // A session that never had a turn is discarded rather than archived
            // (docs/resumption.md), unless its worktree was kept: that session has
            // work outside the log.
            let worktree_gone = !matches!(
                retired,
                Some(crate::worktree::Retired::Kept { .. } | crate::worktree::Retired::Failed(_))
            );
            if archiving && worktree_gone && hosted.session.log_is_empty() {
                let directories = hosted.session.directories();
                match crate::discard::remove(directories.project(), &id, &ids) {
                    Ok(()) => {
                        hosted.session.close_log();
                        log::info!("session {id}: discarded, it never had a turn");
                        return Ended {
                            retired,
                            discarded: true,
                        };
                    }
                    Err(e) => log::warn!("session {id}: could not discard: {e}"),
                }
            }
            // A deliberate archive is recorded last, so a restarted host leaves the
            // session dormant (docs/resumption.md); a host shutdown records nothing.
            if archiving && let Err(e) = hosted.session.append_archived(archived_by) {
                log::warn!("session {id}: could not log the archive: {e}");
            }
            // The archive record is the last append. Closing here rather than at
            // the `Session`'s drop is what lets a resume follow the archive at once:
            // an open SSE stream or a handler can still be holding the session.
            hosted.session.close_log();
            // Announced only for a deliberate archive: a host shutdown cascades
            // sessions through here with `archiving` false, and a dying host has
            // nothing to announce to.
            if archiving {
                log::info!("session {id}: archived");
            }
            Ended {
                retired,
                discarded: false,
            }
        }
    }
}

/// Retire the worktree `session` made for itself (docs/worktrees.md), if it did:
/// its working directory is the one [`crate::worktree::path`] names for its id. A
/// session that inherited a worktree works inside one it did not make and leaves it.
/// Git runs on a blocking thread; the outcome is appended to the session's log, which
/// outlives the runtime's shutdown because the `Session` owns it.
async fn retire_worktree(session: &Arc<Session>, id: &str) -> Option<crate::worktree::Retired> {
    let directories = session.directories();
    let project = directories.project().to_path_buf();
    if directories.working() != crate::worktree::path(&project, id) {
        return None;
    }
    let env = session.child_env();
    let (git_project, git_id) = (project.clone(), id.to_string());
    let retired =
        tokio::task::spawn_blocking(move || crate::worktree::retire(&git_project, &git_id, &env))
            .await
            .unwrap_or_else(|e| crate::worktree::Retired::Failed(format!("worktree task: {e}")));
    if let Some(event) = retired.clone().into_event(&project, id)
        && let Err(e) = session.append(0, crate::log::Payload::Worktree(event))
    {
        log::warn!("session {id}: could not log the worktree outcome: {e}");
    }
    (retired != crate::worktree::Retired::Absent).then_some(retired)
}
