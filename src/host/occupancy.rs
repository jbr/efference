//! Who occupies a managed worktree, and when its last occupant has left
//! (docs/worktrees.md): the count every release point takes before a tree is
//! retired — an archive (`end.rs`) and a start that left the map
//! (`preparation.rs`). A dormant session leaves no other way: it is archived by
//! being resumed first.

use std::path::Path;

use super::*;
use crate::config::env_table::ChildEnv;
use crate::record::session_dir::SESSION_BASE;

/// Whether anything still occupies the managed worktree whose root is `root`: a
/// session on the map — starting, or hosted and not leaving — whose working
/// directory is at or beneath the root, or a dormant session directory under
/// `<project>/.efference` whose header places its working directory there and whose
/// log does not end in an archive (docs/resumption.md).
///
/// A session being archived is counted out by both halves at once: its entry is
/// leaving (`end.rs`, `Departure::leaving`) from the moment its end begins until it
/// has left the map, and while it is on the map its directory is not dormant; once
/// it has left, its log ends in the archive. So when two occupants are archived at
/// once, each recount treats the other session as gone, and both may attempt the
/// removal: the root's lock (`locks.rs`) serializes them, and the second finds the
/// tree `Absent`.
pub(crate) fn remains(sessions: &Sessions, project: &Path, root: &Path) -> bool {
    let in_root = |working: &Path| working == root || working.starts_with(root);
    if sessions
        .starting_iter()
        .any(|starting| in_root(starting.working()))
    {
        return true;
    }
    if sessions.hosted_iter().any(|(_, hosted)| {
        !hosted.departure.leaving() && in_root(hosted.session.directories().working())
    }) {
        return true;
    }
    inventory::sitting(&project.join(SESSION_BASE))
        .into_iter()
        .any(|(id, working)| sessions.entry(&id).is_none() && in_root(&working))
}

impl Host {
    /// Retire the managed worktree at `root`, whose `efference/<tree_id>` branch
    /// names it, when nothing else occupies it (docs/worktrees.md). `None` while any
    /// session still works at or beneath the root, and `Some(Absent)` when the root
    /// is already gone. The recount and the git work are one blocking critical
    /// section under the root's lock (`locks.rs`), so a cancelled await cannot cut
    /// it short. A session admitted after the count takes the same lock before it
    /// uses the tree ([`Host::wait_out_removal`], `boot::resume`), so it finds the
    /// removal finished rather than half-done.
    pub(crate) async fn retire_unoccupied(
        self: &Arc<Self>,
        project: &Path,
        root: &Path,
        tree_id: &str,
        env: ChildEnv,
    ) -> Option<worktree::Retired> {
        let host = Arc::clone(self);
        let (project, root, tree_id) = (
            project.to_path_buf(),
            root.to_path_buf(),
            tree_id.to_string(),
        );
        tokio::task::spawn_blocking(move || {
            let lock = host.locks.for_root(&root);
            let _guard = lock
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            // The map lock covers the recount only, never the git work: a session
            // admitted after it waits on the root's lock before using the tree.
            let occupied = remains(
                &host
                    .sessions
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner),
                &project,
                &root,
            );
            if occupied {
                return None;
            }
            // Where a race test runs its competing writer: between the recount and
            // the removal, with this critical section's lock held (process/testing.md).
            #[cfg(test)]
            if let Some(interleaved) = host
                .before_worktree_removal
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .take()
            {
                interleaved();
            }
            Some(worktree::retire(&project, &tree_id, &env))
        })
        .await
        .unwrap_or_else(|e| Some(worktree::Retired::Failed(format!("worktree task: {e}"))))
    }

    /// Wait out a removal of the managed worktree `working` sits at or inside, for a
    /// creation already on the map (docs/worktrees.md). A removal holds the root's
    /// lock from its recount through the git work, so taking the lock now orders this
    /// after any removal that counted before the session was admitted, and no later
    /// one can begin while it is on the map. `Err` when the working directory is gone
    /// afterwards. A resume does the same where it may restore the tree
    /// (`boot::resume`).
    pub(crate) async fn wait_out_removal(&self, working: &Path) -> Result<(), String> {
        let Some((root, _)) = crate::record::session_dir::worktree::root_of(working) else {
            return Ok(());
        };
        let lock = self.locks.for_root(&root);
        let probe = working.to_path_buf();
        let present = tokio::task::spawn_blocking(move || {
            let _guard = lock
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            probe.is_dir()
        })
        .await
        .map_err(|e| format!("worktree task: {e}"))?;
        if present {
            Ok(())
        } else {
            Err(format!(
                "cwd {} is gone: the managed worktree it works in was removed",
                working.display()
            ))
        }
    }

    /// Consider the managed worktree `working` sits at or inside for retirement on
    /// its own task (docs/worktrees.md): the release point for a start that left the
    /// map (`preparation.rs`), which has no archive response to carry the outcome.
    /// What becomes of the tree is
    /// named on the host log; an end records its own outcome on the session's log
    /// (`end.rs`). A host that is shutting down retires nothing.
    pub(crate) fn consider_retirement(self: &Arc<Self>, working: &Path) {
        if !self.swansong.state().is_running() {
            return;
        }
        let Some((root, tree_id)) = crate::record::session_dir::worktree::root_of(working) else {
            return;
        };
        let Some(project) = crate::record::session_dir::worktree::project_of(&root) else {
            return;
        };
        let host = Arc::clone(self);
        tokio::spawn(async move {
            match host
                .retire_unoccupied(&project, &root, &tree_id, ChildEnv::default())
                .await
            {
                Some(worktree::Retired::Removed { branch_deleted }) => log::info!(
                    "worktree {}: removed at last occupant ({})",
                    root.display(),
                    if branch_deleted {
                        "branch deleted"
                    } else {
                        "branch kept"
                    }
                ),
                Some(worktree::Retired::Kept { reason }) => {
                    log::info!("worktree {}: kept: {reason}", root.display());
                }
                Some(worktree::Retired::Failed(error)) => {
                    log::warn!("worktree {}: could not retire: {error}", root.display());
                }
                Some(worktree::Retired::Absent) | None => {}
            }
        });
    }
}

#[cfg(test)]
mod test;
