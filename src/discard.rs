//! Sessions that never had a turn are removed, not kept (docs/resumption.md).
//!
//! A log is *empty* when it holds no `User` and no `Assistant` entry: the preamble,
//! telemetry, the notifications an idle triage touched, and `Archived` are all
//! activity of the harness, so a session with only those has lost nothing the
//! operator wrote or the agent did. Two paths remove one: archiving a hosted
//! session ([`crate::daemon::Host::end`]) and the boot sweep ([`sweep`]) over a
//! previous host's crash leftovers.

use crate::log::{LogEntry, Payload};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

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
    if !crate::daemon::tcp::plain_id(id) {
        return Err(std::io::Error::other(format!("{id:?} is not a plain id")));
    }
    let dir = root.join(crate::daemon::SESSION_BASE).join(id);
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

/// Remove every empty session log under `roots` that was last written before
/// `booted`: the leftovers of a host that stopped between a session's creation
/// and its first turn. A log written at or after `booted` may belong to a session
/// this host is already running, so it is skipped. A log that does not parse is
/// logged and left. Returns the ids removed.
pub fn sweep(
    roots: &[PathBuf],
    booted: SystemTime,
    ids: &crate::session_ids::SessionIds,
) -> Vec<String> {
    let mut removed = Vec::new();
    for root in roots {
        let base = root.join(crate::daemon::SESSION_BASE);
        let Ok(entries) = std::fs::read_dir(&base) else {
            continue;
        };
        for entry in entries.flatten() {
            let Ok(id) = entry.file_name().into_string() else {
                continue;
            };
            let log = entry.path().join(crate::boot::LOG_NAME);
            if !crate::daemon::tcp::plain_id(&id)
                || !entry.file_type().is_ok_and(|kind| kind.is_dir())
            {
                continue;
            }
            let Ok(meta) = std::fs::metadata(&log) else {
                continue;
            };
            if !meta.is_file() || meta.modified().is_ok_and(|modified| modified >= booted) {
                continue;
            }
            match crate::persist::load(&log) {
                Ok(entries) if is_empty(&entries) => match remove(root, &id, ids) {
                    Ok(()) => {
                        log::info!("session {id}: discarded at boot, it never had a turn");
                        removed.push(id);
                    }
                    Err(e) => log::warn!("session {id}: could not discard: {e}"),
                },
                Ok(_) => {}
                Err(e) => log::warn!("session {id}: log left alone, it did not parse: {e}"),
            }
        }
    }
    removed
}

#[cfg(test)]
mod test;
