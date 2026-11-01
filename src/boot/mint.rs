//! What a creation attempt mints, and the exclusive creation of it (docs/control-channel.md).

use std::path::{Path, PathBuf};

use crate::inference::api::Error;
use crate::record::session_dir::LOG_NAME;

#[cfg(test)]
tokio::task_local! {
    /// The scratch dirs [`mint_scratch_dir`] made inside a test's scope: what a rollback
    /// test checks, where a scan of the system temp dir would also see the scratch of
    /// every test running beside it.
    pub(super) static MINTED_SCRATCH: std::cell::RefCell<Vec<PathBuf>>;
}

/// What this creation attempt minted, for rollback (docs/control-channel.md).
///
/// A path is recorded here only after an exclusive `create_dir` returned success,
/// which is what makes it this attempt's: a dir an existing session or a racing
/// attempt holds makes the create fail, so it is never recorded and never removed.
/// A boot that succeeds hands these to the session instead ([`Minted::keep`]); any
/// other end rolls them back when this drops — a failure, and also a preparation
/// abandoned mid-await, which is how archiving a starting session cancels it
/// (docs/resumption.md).
#[derive(Default)]
pub(super) struct Minted {
    pub(super) session_dir: Option<PathBuf>,
    pub(super) scratch: Option<PathBuf>,
    /// `--keep-scratch`, honored on a failed boot as it is at session end
    /// (docs/tools.md): the scratch directory survives the rollback.
    pub(super) keep_scratch: bool,
    pub(super) worktree: Option<MintedWorktree>,
}

/// A worktree this attempt created, and what removing it needs.
pub(super) struct MintedWorktree {
    pub(super) project: PathBuf,
    pub(super) path: PathBuf,
    pub(super) branch: String,
    pub(super) env: crate::config::env_table::ChildEnv,
}

impl Minted {
    /// Hand what this attempt minted to the session it booted: nothing is removed.
    pub(super) fn keep(mut self) {
        self.worktree = None;
        self.session_dir = None;
        self.scratch = None;
    }

    /// Remove what this attempt minted, naming anything that resists on the log
    /// (`warn`). A failed removal is not raised further: the
    /// preparation error the caller is already returning is the one that matters.
    fn roll_back(&mut self) {
        if let Some(w) = self.worktree.take() {
            crate::worktree::remove(&w.project, &w.path, &w.branch, &w.env);
        }
        if let Some(dir) = self.session_dir.take()
            && let Err(e) = std::fs::remove_dir_all(&dir)
            && e.kind() != std::io::ErrorKind::NotFound
        {
            log::warn!("session dir: could not remove {}: {e}", dir.display());
        }
        match (self.scratch.take(), self.keep_scratch) {
            (Some(scratch), true) => log::debug!("scratch kept: {}", scratch.display()),
            (Some(scratch), false) => {
                if let Err(e) = std::fs::remove_dir_all(&scratch)
                    && e.kind() != std::io::ErrorKind::NotFound
                {
                    log::warn!("scratch: could not remove {}: {e}", scratch.display());
                }
            }
            (None, _) => {}
        }
    }
}

impl Drop for Minted {
    fn drop(&mut self) {
        self.roll_back();
    }
}

/// Create the session dir, refusing a name that already has one (docs/control-channel.md).
///
/// A session does not outlive its process: a second run over the same directory would open
/// the log with an empty in-memory `entries` and restart `seq` at 0 inside a file that
/// already numbers its entries — which every offline reader (`view`, `wire`, the TUI fold)
/// reads as a single sequence. Refusing the occupied directory is what keeps that unreachable
/// on the create path; resuming is its own entry (`boot::resume`), which reopens the log
/// with `Log::open` and never mints. `Log::create` refusing an existing log file for the same
/// reason backs this up (docs/log-and-projection.md).
///
/// `create_dir` rather than `create_dir_all` on the leaf: the leaf creation is the exclusive
/// one, so two creations racing the same name cannot both pass the check. The parent
/// (`.efference/`) is shared by every session: it is created owner-only when absent and
/// tightened when it exists (`daemon::permissions`, docs/state-store.md).
pub fn mint_session_dir(session_dir: &Path) -> Result<(), Error> {
    if let Some(parent) = session_dir.parent() {
        crate::daemon::permissions::ensure_project_base(parent)?;
    }
    use std::os::unix::fs::DirBuilderExt;
    match std::fs::DirBuilder::new()
        .mode(crate::record::session_dir::DIR_MODE)
        .create(session_dir)
    {
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
            Ok(()) => {
                #[cfg(test)]
                let _ =
                    MINTED_SCRATCH.try_with(|minted| minted.borrow_mut().push(candidate.clone()));
                return Ok(candidate);
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }
    Err(std::io::Error::other(
        "could not mint a scratch dir (16 name collisions)",
    ))
}
