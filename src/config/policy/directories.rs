//! The two directories path classification resolves against (docs/permissions.md).

use std::path::{Path, PathBuf};

use super::paths::{absolutize, lexical_normalize};

/// Where a session's relative paths resolve. `[paths]` grant keys are written in the
/// project's `efference.toml`, so they resolve against the **project**; the model's
/// paths resolve against its **working** directory, where its tools run. The two are
/// the same directory unless the session works in a git worktree (`crate::host::worktree`).
///
/// A target inside any worktree is judged as its project counterpart
/// ([`crate::record::session_dir::worktree::counterpart`]), for every session: each worktree
/// carries exactly the project's grants. Only classification sees the counterpart; the
/// tool still touches the path the model named.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Directories {
    project: PathBuf,
    working: PathBuf,
}

impl Directories {
    /// Both directories absolute; `working` is `project` for a session without a worktree.
    pub fn new(project: PathBuf, working: PathBuf) -> Self {
        Self { project, working }
    }

    /// A session whose working directory is its project.
    pub fn at(project: &Path) -> Self {
        Self::new(project.to_path_buf(), project.to_path_buf())
    }

    pub fn project(&self) -> &Path {
        &self.project
    }

    pub fn working(&self) -> &Path {
        &self.working
    }

    /// A path the model named, as classification judges it: resolved against the
    /// working directory, normalized, and mapped to its project counterpart when it is
    /// inside a worktree.
    pub fn target(&self, path: &str) -> PathBuf {
        self.judged(&lexical_normalize(&absolutize(path, &self.working)))
    }

    /// [`Self::target`] for a path already absolute and normalized: a bash redirection
    /// resolved against a `cd`'s directory rather than the working directory.
    pub fn judged(&self, path: &Path) -> PathBuf {
        crate::record::session_dir::worktree::counterpart(&self.project, path)
            .unwrap_or_else(|| path.to_path_buf())
    }

    /// The working directory as classification judges it: the project, for a session
    /// in a worktree. A reviewer shown the worktree's real path reads every target in
    /// it as under the denied `.efference/`, though each is judged as its counterpart.
    pub fn judged_working(&self) -> PathBuf {
        self.judged(&self.working)
    }

    /// A `[paths]` or `[context]` key, resolved against the project.
    pub fn grant(&self, key: &str) -> PathBuf {
        lexical_normalize(&absolutize(key, &self.project))
    }
}
