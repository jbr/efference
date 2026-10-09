//! Where a session's git worktree lives, and the project path a worktree path stands
//! for (docs/permissions.md). The two decisions about worktree placement and access are
//! made here and nowhere else, so either can change in one place.

use std::path::{Component, Path, PathBuf};

/// The directory under the session base that holds every session's worktree.
const WORKTREES: &str = "worktrees";

/// A session's worktree: `<project>/.efference/worktrees/<id>`. Flat, beside the
/// session dirs rather than inside one, so one prefix covers every worktree.
pub fn path(project: &Path, id: &str) -> PathBuf {
    project
        .join(crate::daemon::SESSION_BASE)
        .join(WORKTREES)
        .join(id)
}

/// The project path a worktree path stands for: `<project>/.efference/worktrees/<id>/<rest>`
/// is `<project>/<rest>`, and a worktree's own directory is `<project>`. `None` for a
/// path inside no worktree, including the `worktrees` directory itself.
///
/// The permission classifier judges a path inside any worktree as this counterpart, so
/// every worktree carries exactly the project's grants, narrower ones included
/// (docs/permissions.md). Lexical, like the rest of path classification: `path` must
/// already be absolute and normalized.
pub fn counterpart(project: &Path, path: &Path) -> Option<PathBuf> {
    let worktrees = project.join(crate::daemon::SESSION_BASE).join(WORKTREES);
    let rest = path.strip_prefix(&worktrees).ok()?;
    let mut components = rest.components();
    match components.next() {
        Some(Component::Normal(_)) => Some(project.join(components.as_path())),
        _ => None,
    }
}

#[cfg(test)]
mod test;
