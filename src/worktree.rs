//! Where a session's git worktree lives, and the project path a worktree path stands
//! for (docs/permissions.md). The two decisions about worktree placement and access are
//! made here and nowhere else, so either can change in one place.

use std::path::{Component, Path, PathBuf};
use std::process::{Command, Output};

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

/// The project a directory inside some worktree belongs to: the prefix before its
/// `.efference/worktrees/<id>`, or `None` for a directory inside no worktree. A session
/// created in such a directory is rooted at that project and works in the directory, so
/// a session created from inside a worktree, by a person or by a session working there,
/// inherits it (docs/control-channel.md). Lexical; the innermost match wins.
pub fn project_of(dir: &Path) -> Option<PathBuf> {
    let components: Vec<Component> = dir.components().collect();
    (0..components.len().saturating_sub(2)).rev().find_map(|i| {
        let is_worktree = components[i].as_os_str() == crate::daemon::SESSION_BASE
            && components[i + 1].as_os_str() == WORKTREES
            && matches!(components[i + 2], Component::Normal(_));
        is_worktree.then(|| components[..i].iter().collect())
    })
}

/// What `git worktree add` produced.
#[derive(Debug)]
pub struct Added {
    pub path: PathBuf,
    pub branch: String,
    /// The commit the branch starts at: the creator's `HEAD`.
    pub base: String,
    /// Git's exit status. Nonzero with the directory present is a failed
    /// `post-checkout` hook: git returns the hook's status as its own.
    pub exit_code: Option<i32>,
    /// Git's stdout and stderr, truncated to [`OUTPUT_CAP`].
    pub output: String,
}

/// The most of git's output kept.
const OUTPUT_CAP: usize = 64 * 1024;

/// A session's branch: `efference/<id>`.
pub fn branch(id: &str) -> String {
    format!("efference/{id}")
}

fn git(dir: &Path, env: &crate::env_table::ChildEnv, args: &[&str]) -> Result<Output, String> {
    let mut cmd = Command::new("git");
    cmd.args(args).current_dir(dir);
    env.apply(&mut cmd);
    unset_repository_env(&mut cmd);
    crate::spawn::std_output(&mut cmd).map_err(|e| format!("could not run git: {e}"))
}

/// Git's repository-local variables, as `git rev-parse --local-env-vars` lists them.
const REPOSITORY_ENV: &[&str] = &[
    "GIT_ALTERNATE_OBJECT_DIRECTORIES",
    "GIT_CONFIG",
    "GIT_CONFIG_PARAMETERS",
    "GIT_CONFIG_COUNT",
    "GIT_OBJECT_DIRECTORY",
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_IMPLICIT_WORK_TREE",
    "GIT_GRAFT_FILE",
    "GIT_INDEX_FILE",
    "GIT_NO_REPLACE_OBJECTS",
    "GIT_REPLACE_REF_BASE",
    "GIT_PREFIX",
    "GIT_SHALLOW_FILE",
    "GIT_COMMON_DIR",
];

/// Remove git's repository-local variables from `cmd`, so git finds its repository
/// from the working directory alone. Git exports them to the hooks it runs: an
/// environment captured inside one (a host started from a hook, a test run by the
/// pre-commit hook) carries `GIT_DIR` naming that other repository, and a `git init`
/// under it reinitializes that repository as bare.
pub(crate) fn unset_repository_env(cmd: &mut Command) {
    for name in REPOSITORY_ENV {
        cmd.env_remove(name);
    }
}

/// Stdout and stderr together, truncated at a character boundary with a marker.
fn combined(output: &Output) -> String {
    let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&output.stderr));
    if text.len() > OUTPUT_CAP {
        let mut end = OUTPUT_CAP;
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        text.truncate(end);
        text.push_str("\n[output truncated]");
    }
    text
}

/// Create the session's worktree: `git worktree add -b efference/<id> <path> <base>`,
/// run in `from` (the creator's working directory) with the session's child
/// environment, on the commit `from`'s `HEAD` names (docs/control-channel.md). The
/// checkout is plain, so the project's `post-checkout` hook runs.
///
/// The directory existing afterward is the success criterion. A hook that fails makes
/// git exit nonzero with the tree in place; that returns `Ok` with the status and
/// output recorded.
pub fn add(
    project: &Path,
    id: &str,
    from: &Path,
    env: &crate::env_table::ChildEnv,
) -> Result<Added, String> {
    let inside = git(from, env, &["rev-parse", "--is-inside-work-tree"])?;
    if !inside.status.success() || String::from_utf8_lossy(&inside.stdout).trim() != "true" {
        return Err(format!(
            "worktree: true needs a git work tree, and {} is not inside one",
            from.display()
        ));
    }
    let head = git(from, env, &["rev-parse", "HEAD"])?;
    if !head.status.success() {
        return Err(format!(
            "git could not resolve HEAD in {}: {}",
            from.display(),
            combined(&head).trim()
        ));
    }
    let base = String::from_utf8_lossy(&head.stdout).trim().to_string();
    let path = path(project, id);
    let branch = branch(id);
    let added = git(
        from,
        env,
        &[
            "worktree",
            "add",
            "-b",
            &branch,
            &path.to_string_lossy(),
            &base,
        ],
    )?;
    let output = combined(&added);
    if !path.is_dir() {
        return Err(format!("git worktree add failed: {}", output.trim()));
    }
    Ok(Added {
        path,
        branch,
        base,
        exit_code: added.status.code(),
        output,
    })
}

/// Undo [`add`] after a failed boot: remove the worktree, then its branch. Best effort;
/// a failure is named on stderr, the boot report's surface.
pub fn remove(project: &Path, path: &Path, branch: &str, env: &crate::env_table::ChildEnv) {
    let path = path.to_string_lossy();
    for args in [
        vec!["worktree", "remove", "--force", &path],
        vec!["branch", "-D", branch],
    ] {
        match git(project, env, &args) {
            Ok(output) if output.status.success() => {}
            Ok(output) => eprintln!(
                "[worktree: `git {}` failed: {}]",
                args.join(" "),
                combined(&output).trim()
            ),
            Err(e) => eprintln!("[worktree: {e}]"),
        }
    }
}

#[cfg(test)]
mod test;
