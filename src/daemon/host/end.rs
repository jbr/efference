//! Archiving a session (docs/control-api.md, docs/resumption.md): [`Host::end`] shuts a
//! hosted session down and retires its own worktree, and a session that never had a turn
//! is discarded instead of archived. The session leaves the host's map once its end has
//! recorded it, and the end resolves once it has left ([`Departure`]).

use super::*;
use tokio::sync::watch;

/// What [`Host::end`] did: the outcome for the session's own worktree, and whether
/// the session was discarded instead of archived (docs/resumption.md).
#[derive(Debug)]
pub struct Ended {
    pub retired: Option<crate::worktree::Retired>,
    pub discarded: bool,
}

impl Ended {
    /// The answer to an archive (docs/control-api.md): the `state` it reached,
    /// `archived` or `discarded` when the session was removed, and the fate of its
    /// worktree when it made one. A route adds what only it knows.
    pub(super) fn answer(self, id: &str) -> SessionArchived {
        SessionArchived {
            id: id.to_string(),
            state: if self.discarded {
                ArchiveOutcome::Discarded
            } else {
                ArchiveOutcome::Archived
            },
            resumed: None,
            worktree: self.retired.and_then(|retired| retired.response()),
            ended_with: Vec::new(),
        }
    }
}

impl Host {
    /// Archive one session: shut it down, retire its own worktree when it made one,
    /// and record the archive (docs/control-api.md). Resolves once the session has left
    /// the map, with what became of the worktree (`None` when the session had none, or
    /// its directory was already gone). `None` when no session has that id.
    ///
    /// The order (docs/control-channel.md): the end appends `Archived` and closes the
    /// log; the watcher [`adopt`](Self::adopt) spawned removes the map entry after that,
    /// and after it has ended the session's children; the end resolves after the
    /// removal. Both waits are the entry's [`Departure`]. So a client that no longer finds
    /// the session listed finds the record in its log, and an archive resolves after the
    /// sessions that end with it have left the map.
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
    pub fn end(self: &Arc<Self>, id: &str) -> Option<Pin<Box<dyn Future<Output = Ended> + Send>>> {
        self.end_by(id, None)
    }

    /// [`end`](Self::end) for a caller that names itself: `source` is recorded on the
    /// session's `Archived` entry, spelled as a turn's `source` is (docs/log-and-projection.md).
    /// A session cancelled while it is still starting records none.
    pub fn end_by(
        self: &Arc<Self>,
        id: &str,
        source: Option<String>,
    ) -> Option<Pin<Box<dyn Future<Output = Ended> + Send>>> {
        // One critical section of the map lock reads the entry and counts a hosted
        // session's end on it (`Departure`).
        let sessions = self.sessions.lock().unwrap();
        Some(match sessions.entry(id)? {
            Entry::Hosted(hosted) => Box::pin(self.end_hosted(hosted, id.to_string(), source)),
            Entry::Starting(starting) => {
                Box::pin(Arc::clone(self).archive_starting(Arc::clone(starting), source))
            }
        })
    }

    /// [`end`](Self::end) for a hosted session. `hosted` is borrowed from the map while
    /// its lock is held, and the end is counted on it before the lock is released: the
    /// entry's watcher then either sees this end under way, or had already removed the
    /// entry and nothing found it to end ([`Departure`]).
    pub(super) fn end_hosted(
        &self,
        hosted: &Hosted,
        id: String,
        archived_by: Option<String>,
    ) -> impl Future<Output = Ended> + use<> {
        let under_way = hosted.departure.begin_end();
        let departure = hosted.departure.clone();
        let record = record_end(
            hosted.clone(),
            id,
            archived_by,
            self.swansong.state().is_running(),
            self.session_ids.clone(),
        );
        async move {
            let ended = record.await;
            // Recorded: the watcher may remove the entry now.
            drop(under_way);
            departure.has_left().await;
            ended
        }
    }

    /// The last step of the watcher [`adopt`](Self::adopt) spawned: remove `id`'s hosted
    /// entry once no end under way on it holds the removal back, in the critical section
    /// of the map lock that finds none, then tell the ends waiting on it that it has left
    /// ([`Departure`]).
    pub(super) async fn remove_when_recorded(&self, id: &str, departure: &Departure) {
        loop {
            let removed = self.sessions.lock().unwrap().remove_hosted(id);
            if removed {
                break;
            }
            departure.released().await;
        }
        self.touch_listing();
        departure.mark_left();
    }
}

/// How a hosted session leaves the map (docs/control-channel.md). Each end under way on
/// the session holds the entry's removal back until it has recorded the session — appended
/// `Archived`, or discarded it, and closed its log — and then waits for the removal before
/// it resolves. The watcher [`Host::adopt`] spawned removes the entry; nothing else does.
///
/// An end is counted in the critical section of the map lock that finds the entry, and the
/// watcher removes the entry only in one that finds no end counted, so an end is either
/// waited for or finds no entry to end. Every clone of the entry shares one.
#[derive(Clone, Default)]
pub(super) struct Departure(watch::Sender<DepartureState>);

/// Where a [`Departure`] stands.
#[derive(Default)]
struct DepartureState {
    /// Ends under way on the session that have not yet recorded it.
    ends: usize,
    /// Whether the entry has left the map.
    left: bool,
}

impl Departure {
    /// Count an end under way until the guard it returns drops. Called under the map
    /// lock that found the entry.
    fn begin_end(&self) -> EndUnderWay {
        self.0.send_modify(|state| state.ends += 1);
        EndUnderWay(self.clone())
    }

    /// Whether an end under way holds the removal back. Read under the map lock.
    pub(super) fn held(&self) -> bool {
        self.0.borrow().ends > 0
    }

    /// Resolve once no end under way holds the removal back.
    async fn released(&self) {
        let _ = self.0.subscribe().wait_for(|state| state.ends == 0).await;
    }

    /// The entry has left the map: the ends waiting on it resolve.
    fn mark_left(&self) {
        self.0.send_modify(|state| state.left = true);
    }

    /// Resolve once the entry has left the map.
    async fn has_left(&self) {
        let _ = self.0.subscribe().wait_for(|state| state.left).await;
    }
}

/// One end under way ([`Departure::begin_end`]), holding the entry's removal back until
/// it drops: once the end has recorded the session, or when the end is dropped unfinished.
struct EndUnderWay(Departure);

impl Drop for EndUnderWay {
    fn drop(&mut self) {
        self.0.0.send_modify(|state| state.ends -= 1);
    }
}

/// What an end does before the session may leave the map: shut it down, retire its own
/// worktree when `archiving`, then discard it or record the archive, and close its log.
/// `archiving` is false while the host shuts down, which records nothing.
async fn record_end(
    hosted: Hosted,
    id: String,
    archived_by: Option<String>,
    archiving: bool,
    ids: crate::session_ids::SessionIds,
) -> Ended {
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
    if archiving {
        // Where a test lands the watcher's removal before the record (`before_archived`).
        #[cfg(test)]
        {
            let interleaved = hosted.session.before_archived.lock().unwrap().take();
            if let Some(interleaved) = interleaved {
                interleaved(&hosted.session);
            }
        }
        if let Err(e) = hosted.session.append_archived(archived_by) {
            log::warn!("session {id}: could not log the archive: {e}");
        }
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
        && let Err(e) = session.append(0, Payload::Worktree(event))
    {
        log::warn!("session {id}: could not log the worktree outcome: {e}");
    }
    (retired != crate::worktree::Retired::Absent).then_some(retired)
}
