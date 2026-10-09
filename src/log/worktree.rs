//! The durable record of a session's worktree creation (docs/control-channel.md): the
//! path, the branch, the commit it started from, and what `git worktree add` said.
//!
//! None of it reaches the wire: [`Payload::Worktree`] projects to nothing, like
//! [`Payload::Cold`]. It exists as the audit trail for a cold or failed setup. Git runs
//! the project's `post-checkout` hook during `git worktree add` and makes the hook's exit
//! status its own, so a worktree whose warming failed is a created worktree with a
//! nonzero `exit_code`, and the hook's output is in `output`.
//!
//! [`Payload::Worktree`]: crate::log::Payload::Worktree
//! [`Payload::Cold`]: crate::log::Payload::Cold

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// A worktree the host created for a session.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorktreeAdded {
    /// The worktree's directory.
    pub path: PathBuf,
    /// The branch created for it.
    pub branch: String,
    /// The commit the branch started from: the creator's `HEAD`.
    pub base: String,
    /// Git's exit status; `None` when a signal ended it. Nonzero with an existing
    /// directory is a failed `post-checkout` hook.
    pub exit_code: Option<i32>,
    /// Git's stdout and stderr, truncated.
    pub output: String,
}
