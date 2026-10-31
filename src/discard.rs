//! Sessions that never had a turn are removed, not kept (docs/resumption.md).
//!
//! A log is *empty* when it holds no `User` and no `Assistant` entry: the preamble,
//! telemetry, the notifications an idle triage touched, and `Archived` are all
//! activity of the harness, so a session with only those has lost nothing the
//! operator wrote or the agent did. Archiving a hosted session
//! ([`crate::daemon::Host::end`]) removes one; a host start removes none, so an
//! empty log a crashed host left is reopened as interrupted and discarded when it
//! is archived.

use crate::log::{LogEntry, Payload};
use std::path::Path;

/// Whether `entries` hold no operator turn and no agent turn.
pub fn is_empty(entries: &[LogEntry]) -> bool {
    !entries
        .iter()
        .any(|entry| matches!(entry.payload, Payload::User(_) | Payload::Assistant { .. }))
}

/// Remove a session's directory and release its id.
///
/// Refuses anything that is not a real directory directly under the root's
/// `.efference` named by `id` (a symlink is refused, never followed).
pub fn remove(root: &Path, id: &str, ids: &crate::session_ids::SessionIds) -> std::io::Result<()> {
    if !crate::session_dir::plain_id(id) {
        return Err(std::io::Error::other(format!("{id:?} is not a plain id")));
    }
    let dir = root.join(crate::session_dir::SESSION_BASE).join(id);
    if !std::fs::symlink_metadata(&dir)?.is_dir() {
        return Err(std::io::Error::other(format!(
            "{} is not a directory",
            dir.display()
        )));
    }
    std::fs::remove_dir_all(&dir)?;
    ids.release(id, root);
    Ok(())
}

#[cfg(test)]
mod test;
