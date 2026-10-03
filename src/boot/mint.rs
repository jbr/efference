//! What a creation attempt mints, and the exclusive creation of it (docs/control-channel.md).

use std::path::{Path, PathBuf};

use super::LOG_NAME;
use crate::api::Error;

/// What this creation attempt minted, for rollback (docs/control-channel.md).
///
/// A path is recorded here only after an exclusive `create_dir` returned success,
/// which is what makes it this attempt's: a dir an existing session or a racing
/// attempt holds makes the create fail, so it is never recorded and never removed.
/// A boot that succeeds hands these to the session instead.
#[derive(Default)]
pub(super) struct Minted {
    pub(super) session_dir: Option<PathBuf>,
    pub(super) scratch: Option<PathBuf>,
    /// `--keep-scratch`, honored on a failed boot as it is at session end
    /// (docs/tools.md): the post-mortem artifact survives the rollback.
    pub(super) keep_scratch: bool,
}

impl Minted {
    /// Remove what this attempt minted, naming anything that resists on stderr —
    /// the boot report's surface. A failed removal is not escalated: the
    /// preparation error the caller is already returning is the one that matters.
    pub(super) fn roll_back(self) {
        if let Some(dir) = &self.session_dir
            && let Err(e) = std::fs::remove_dir_all(dir)
            && e.kind() != std::io::ErrorKind::NotFound
        {
            eprintln!("[session dir: could not remove {}: {e}]", dir.display());
        }
        match (&self.scratch, self.keep_scratch) {
            (Some(scratch), true) => eprintln!("scratch kept: {}", scratch.display()),
            (Some(scratch), false) => {
                if let Err(e) = std::fs::remove_dir_all(scratch)
                    && e.kind() != std::io::ErrorKind::NotFound
                {
                    eprintln!("[scratch: could not remove {}: {e}]", scratch.display());
                }
            }
            (None, _) => {}
        }
    }
}

/// Create the session dir, refusing a name that already has one (docs/control-channel.md).
///
/// A session does not outlive its process: a second run over the same directory would open
/// the log with an empty in-memory `entries` and restart `seq` at 0 inside a file that
/// already numbers its entries — which every offline reader (`view`, `wire`, the TUI fold)
/// reads as a single sequence. Refusing the occupied directory is what keeps that unreachable
/// until resumption is designed; `Log::create` refusing an existing log file for the same
/// reason backs it up (docs/log-and-projection.md), and when resumption is, this guard
/// changes with it rather than gaining exceptions.
///
/// `create_dir` rather than `create_dir_all` on the leaf: the leaf creation is the exclusive
/// one, so two creations racing the same name cannot both pass the check. The parent
/// (`.efference/`) is still created recursively — it is shared by every session.
pub fn mint_session_dir(session_dir: &Path) -> Result<(), Error> {
    if let Some(parent) = session_dir.parent() {
        std::fs::create_dir_all(parent)?;
    }
    match std::fs::create_dir(session_dir) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Err(format!(
            "session dir {} already exists — creation mints a new session and does not resume one. \
             Pick another --name; the existing session's log is intact at {}.",
            session_dir.display(),
            session_dir.join(LOG_NAME).display()
        )
        .into()),
        Err(e) => Err(format!(
            "could not create session dir {}: {e}",
            session_dir.display()
        )
        .into()),
    }
}

/// Mint the per-session scratch dir (docs/tools.md): `<system-temp>/efference-<rand>/`,
/// mkdtemp-style — a random name and create-or-retry (`create_dir` fails if the name
/// exists, so there is no check-then-create race), mode `0700` so it is private on a
/// shared-`/tmp` system (macOS `$TMPDIR` is already per-user). `std::env::temp_dir()`
/// rather than a literal `/tmp`, which on macOS is a symlink and not the real temp dir.
pub fn mint_scratch_dir() -> std::io::Result<PathBuf> {
    use std::hash::{BuildHasher, Hasher, RandomState};
    let base = std::env::temp_dir();
    for _ in 0..16 {
        // `RandomState` seeds from OS entropy — an unpredictable-enough name;
        // create-or-retry (not check-then-create) is what closes the race.
        let n = RandomState::new().build_hasher().finish();
        let candidate = base.join(format!("efference-{n:016x}"));
        let mut builder = std::fs::DirBuilder::new();
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        match builder.create(&candidate) {
            Ok(()) => return Ok(candidate),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }
    Err(std::io::Error::other(
        "could not mint a scratch dir (16 name collisions)",
    ))
}
