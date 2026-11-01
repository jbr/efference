//! Where a session's git worktree lives, and the project path a worktree path stands
//! for (docs/worktrees.md). The two decisions about worktree placement and access are
//! made here and nowhere else, so either can change in one place. The git operations
//! on the worktree are [`crate::worktree`].
//!
//! Lexical: nothing here reads the filesystem, and, like its parent, the module imports
//! nothing from the crate.

use std::path::{Component, Path, PathBuf};

use super::SESSION_BASE;

/// The directory under the session base that holds every session's worktree.
const WORKTREES: &str = "worktrees";

/// A session's worktree: `<project>/.efference/worktrees/<id>`. Flat, beside the
/// session dirs rather than inside one, so one prefix covers every worktree.
pub fn path(project: &Path, id: &str) -> PathBuf {
    project.join(SESSION_BASE).join(WORKTREES).join(id)
}

/// The project path a worktree path stands for: `<project>/.efference/worktrees/<id>/<rest>`
/// is `<project>/<rest>`, and a worktree's own directory is `<project>`. `None` for a
/// path inside no worktree, including the `worktrees` directory itself.
///
/// The permission classifier judges a path inside any worktree as this counterpart, so
/// every worktree carries exactly the project's grants, narrower ones included
/// (docs/worktrees.md). Lexical, like the rest of path classification: `path` must
/// already be absolute and normalized.
pub fn counterpart(project: &Path, path: &Path) -> Option<PathBuf> {
    let worktrees = project.join(SESSION_BASE).join(WORKTREES);
    let rest = path.strip_prefix(&worktrees).ok()?;
    let mut components = rest.components();
    match components.next() {
        // `join("")` would append a separator, which a displayed path keeps.
        Some(Component::Normal(_)) if components.as_path().as_os_str().is_empty() => {
            Some(project.to_path_buf())
        }
        Some(Component::Normal(_)) => Some(project.join(components.as_path())),
        _ => None,
    }
}

/// The project a directory inside some worktree belongs to: the prefix before its
/// `.efference/worktrees/<id>`, or `None` for a directory inside no worktree. A session
/// created in such a directory is rooted at that project and works in the directory, so
/// a session created from inside a worktree, by a person or by a session working there,
/// inherits it (docs/worktrees.md). Lexical; the innermost match wins.
pub fn project_of(dir: &Path) -> Option<PathBuf> {
    let components: Vec<Component> = dir.components().collect();
    (0..components.len().saturating_sub(2)).rev().find_map(|i| {
        let is_worktree = components[i].as_os_str() == SESSION_BASE
            && components[i + 1].as_os_str() == WORKTREES
            && matches!(components[i + 2], Component::Normal(_));
        is_worktree.then(|| components[..i].iter().collect())
    })
}

#[cfg(test)]
mod test;
