//! Owner-only file modes on the host's on-disk state, tightened at boot
//! (docs/state-store.md, docs/control-channel.md).
//!
//! Files under a permissive umask arrive group- and world-readable, and a
//! socket arrives connectable by any local account; boot tightens what it
//! finds to owner-only so the creating process's default does not decide the
//! outcome. Tightening is one-way — a file already stricter stays as its mode
//! is — because a mode the operator set deliberately is not the daemon's to
//! widen.

use std::path::Path;

/// Owner-only mode for the state directory and the host socket: nothing
/// another local account can traverse or connect to.
const DIR_MODE: u32 = 0o700;

/// Owner read/write for the state files.
const FILE_MODE: u32 = 0o600;

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
