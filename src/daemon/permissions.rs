//! Owner-only file modes on the host's on-disk state, tightened at boot
//! (docs/state-store.md, docs/control-channel.md).
//!
//! Files under a permissive umask arrive group- and world-readable, and a
//! socket arrives connectable by any local account; boot tightens what it
//! finds to owner-only so the creating process's default does not decide the
//! outcome. Tightening is one-way — a file already stricter stays as its mode
//! is — because a mode the operator set deliberately is not the daemon's to
//! widen.
//!
//! A project's `.efference/` gets the same owner-only modes from the sessions
//! that use it, not from boot ([`ensure_project_base`], [`tighten_project_base`]):
//! the directory's mode is what protects the session directories, logs, usage
//! records, attachments, and worktrees beneath it, and a worktree's checked-out
//! files keep the modes git gave them (docs/state-store.md).

use std::path::Path;

/// Owner-only mode for the state directory and the host socket: nothing
/// another local account can traverse or connect to.
pub(crate) const DIR_MODE: u32 = 0o700;

/// Owner read/write for the state files.
pub(crate) const FILE_MODE: u32 = 0o600;

/// The files beside the control socket that hold user state or secrets.
/// `state.db` is the current store (the name of `crate::state::RELATIVE_PATH`)
/// and `config.toml` the user configuration; the rest were written by earlier
/// releases and no longer are, named so a machine still carrying one is
/// tightened too.
const STATE_FILES: [&str; 5] = [
    "state.db",
    "config.toml",
    "push.json",
    "state.db.bak-seed",
    "totp-burns",
];

/// Tighten the state directory and the state files beside it — the host's
/// whole on-disk footprint beside the socket. Absent files are skipped; a
/// file that cannot be tightened warns and boot continues, because a mode is
/// hardening rather than a precondition for serving.
pub(crate) fn tighten_state_dir(dir: &Path) {
    tighten(dir, DIR_MODE);
    for name in STATE_FILES {
        tighten(&dir.join(name), FILE_MODE);
    }
}

/// Tighten the host socket itself, after the bind mints it under the process
/// umask. Owner-only is exactly the accounts that may reach the control
/// channel: connecting to a socket needs write permission.
pub(crate) fn tighten_socket(socket: &Path) {
    tighten(socket, DIR_MODE);
}

/// Make sure a project's `.efference/` exists and is owner-only before a session
/// directory is made inside it: created `0700` when absent, tightened when an
/// earlier run, or another tool, left it more open. The project directory
/// above it is created if missing and otherwise left as it is.
pub(crate) fn ensure_project_base(base: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::DirBuilderExt;
    if let Some(project) = base.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(project)?;
    }
    match std::fs::DirBuilder::new().mode(DIR_MODE).create(base) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(e) => return Err(e),
    }
    tighten_project_base(base);
    Ok(())
}

/// Tighten an existing project `.efference/` — what a session opening one does.
/// Its contents are not walked: the directory's mode is the boundary.
pub(crate) fn tighten_project_base(base: &Path) {
    tighten(base, DIR_MODE);
}

/// Tighten a file efference just wrote that it did not create with
/// [`FILE_MODE`] (a copy carries its source's mode; a plain write takes the umask's).
pub(crate) fn tighten_file(path: &Path) {
    tighten(path, FILE_MODE);
}

/// Intersect `path`'s current mode with `target`, so a stricter mode — a file
/// the operator locked to `0400` — is kept rather than widened.
fn tighten(path: &Path, target: u32) {
    use std::os::unix::fs::PermissionsExt;
    let mode = match std::fs::metadata(path) {
        Ok(meta) => meta.permissions().mode() & 0o777,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return,
        Err(e) => {
            log::warn!("could not tighten {}: {e}", path.display());
            return;
        }
    };
    let tightened = mode & target;
    if tightened == mode {
        return;
    }
    if let Err(e) = std::fs::set_permissions(path, std::fs::Permissions::from_mode(tightened)) {
        log::warn!("could not tighten {}: {e}", path.display());
        return;
    }
    log::info!(
        "tightened {} from {:o} to {:o}",
        path.display(),
        mode,
        tightened
    );
}

#[cfg(test)]
mod test;
