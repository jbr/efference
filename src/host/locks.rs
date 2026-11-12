//! Per-root locks for the managed-worktree lifecycle (docs/worktrees.md): every
//! check of who occupies a managed root, and every git operation that creates,
//! restores, or removes one, runs under the root's lock — so a session admitted
//! while a root is being removed finds the removal finished before it uses the
//! tree, two restores cannot race one checkout, and an add cannot meet a removal
//! half-done.
//!
//! Lock order: a root lock is taken before the sessions map lock
//! (`entries.rs`), never the reverse; at most one root lock is held at a time,
//! because a session's innermost managed root is unique
//! (`crate::record::session_dir::worktree::root_of`). Each critical section acquires
//! its guard inside the `spawn_blocking` closure that does the work: a cancelled
//! await must not release the guard while the git work it guards is still running,
//! and no async thread waits while a `post-checkout` hook or a removal runs. The one
//! exception is a failed creation's rollback, which takes its own new root's lock as
//! it drops (`boot::mint`). A lock is per root, so one slow checkout delays only the
//! operations on that root.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// One lock per managed root path, handed out on demand.
#[derive(Default)]
pub struct Locks(Mutex<HashMap<PathBuf, Arc<Mutex<()>>>>);

impl Locks {
    /// The lock guarding the managed root at `root`: the same handle for every
    /// caller, so the operations above serialize on it.
    pub fn for_root(&self, root: &Path) -> Arc<Mutex<()>> {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .entry(root.to_path_buf())
            .or_default()
            .clone()
    }
}

#[cfg(test)]
mod test;
