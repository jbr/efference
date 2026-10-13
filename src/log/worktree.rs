//! The durable record of a session's worktree life (docs/worktrees.md,
//! docs/log-and-projection.md): its creation, its re-creation on resume, and its
//! removal or retention on archive.
//!
//! None of it reaches the wire: [`Payload::Worktree`] projects to nothing, like
//! [`Telemetry::Cold`]. It exists as the audit trail for a cold or failed setup and for
//! what became of the directory afterward. Git runs the project's `post-checkout` hook
//! during `git worktree add` and makes the hook's exit status its own, so a worktree
//! whose warming failed is a created worktree with a nonzero `exit_code`, and the
//! hook's output is in `output`.
//!
//! [`Payload::Worktree`]: crate::log::Payload::Worktree
//! [`Telemetry::Cold`]: crate::log::Telemetry::Cold

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// One event in a session's own worktree.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorktreeEvent {
    /// The worktree's directory.
    pub path: PathBuf,
    /// The session's branch.
    pub branch: String,
    pub kind: WorktreeEventKind,
}

/// What happened to the worktree.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum WorktreeEventKind {
    /// Created at session creation, on a new branch.
    Added(Checkout),
    /// Re-created on resume, from the kept branch or a fresh one at the project's `HEAD`.
    Restored(Checkout),
    /// Removed on archive because it held no uncommitted work.
    Removed {
        /// Whether the branch was deleted too: only when another branch contains its tip.
        branch_deleted: bool,
    },
    /// Left in place on archive.
    Kept {
        /// Why: the uncommitted work, summarized.
        reason: String,
    },
    /// Git failed to inspect or remove it on archive.
    RemoveFailed {
        /// Git's message.
        error: String,
    },
}

/// What `git worktree add` produced.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Checkout {
    /// The commit the checkout is on: the creator's `HEAD` for an add; the branch tip
    /// or the project's `HEAD` for a restore.
    pub base: String,
    /// Git's exit status; `None` when a signal ended it. Nonzero with an existing
    /// directory is a failed `post-checkout` hook.
    pub exit_code: Option<i32>,
    /// Git's stdout and stderr, truncated.
    pub output: String,
}
