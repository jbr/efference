//! Whether a path holds a managed worktree's own checkout (docs/worktrees.md): what
//! an add or a restore must leave behind to succeed, and what a removal or a
//! retirement must find before it deletes anything.

use std::path::Path;
use std::process::Output;

use super::{Added, branch, combined, git, git_ok};
use crate::config::env_table::ChildEnv;

/// The outcome of a `git worktree add` that was asked for `branch` at `path`: this
/// tree's checkout ([`checkout_is`]) whatever git's exit status — a nonzero one with
/// the checkout in place is a failed `post-checkout` hook, recorded with its output —
/// or an error that leaves what git made untouched (docs/worktrees.md).
pub(super) fn checked(
    project: &Path,
    path: std::path::PathBuf,
    branch: String,
    base: String,
    added: &Output,
    env: &ChildEnv,
) -> Result<Added, String> {
    let output = combined(added);
    match checkout_is(project, &path, &branch, env) {
        Ok(true) => Ok(Added {
            path,
            branch,
            base,
            exit_code: added.status.code(),
            output,
        }),
        Ok(false) => Err(format!(
            "git worktree add left no checkout of {branch} at {}: {}",
            path.display(),
            output.trim()
        )),
        Err(reading) => Err(format!(
            "git worktree add failed: {}; and the checkout left behind cannot be read: {reading}",
            output.trim()
        )),
    }
}

/// The branch `path`'s `HEAD` is on, or `None` when it is detached.
pub(super) fn head_branch(path: &Path, env: &ChildEnv) -> Result<Option<String>, String> {
    let head = git(path, env, &["symbolic-ref", "-q", "--short", "HEAD"])?;
    Ok(head
        .status
        .success()
        .then(|| String::from_utf8_lossy(&head.stdout).trim().to_string()))
}

/// Whether `project`'s git has a worktree registered at exactly `path`
/// (docs/worktrees.md).
pub(super) fn registered(project: &Path, path: &Path, env: &ChildEnv) -> Result<bool, String> {
    let listed = git_ok(project, env, &["worktree", "list", "--porcelain"])?;
    Ok(listed
        .lines()
        .filter_map(|line| line.strip_prefix("worktree "))
        .any(|registered| same_path(Path::new(registered), path)))
}

/// Whether two paths are the same directory: equal as given, or equal after
/// canonicalization when both resolve (git prints resolved paths).
fn same_path(a: &Path, b: &Path) -> bool {
    if a == b {
        return true;
    }
    matches!(
        (std::fs::canonicalize(a), std::fs::canonicalize(b)),
        (Ok(a), Ok(b)) if a == b
    )
}

/// Whether `path` holds this tree's checkout: `project`'s git registered a worktree
/// at exactly `path`, its git dir is usable, and its `HEAD` is `branch`. The success
/// criterion for [`add`](super::add) and [`restore`](super::restore) (docs/worktrees.md).
pub(super) fn checkout_is(
    project: &Path,
    path: &Path,
    branch: &str,
    env: &ChildEnv,
) -> Result<bool, String> {
    if !registered(project, path, env)? {
        return Ok(false);
    }
    git_ok(path, env, &["rev-parse", "--git-dir"])?;
    Ok(head_branch(path, env)?.is_some_and(|on| on == branch))
}

/// Like [`checkout_is`], but a `HEAD` detached at a commit `branch` contains also
/// counts: the checkout is still this tree's, which is what [`remove`](super::remove) may delete
/// (docs/worktrees.md).
pub(super) fn owned_checkout(
    project: &Path,
    path: &Path,
    branch: &str,
    env: &ChildEnv,
) -> Result<bool, String> {
    if !checkout_is(project, path, branch, env)? {
        if !registered(project, path, env)? || head_branch(path, env)?.is_some() {
            return Ok(false);
        }
        let holders = git_ok(
            path,
            env,
            &["branch", "--contains", "HEAD", "--format=%(refname:short)"],
        )?;
        return Ok(holders.lines().any(|line| line.trim() == branch));
    }
    Ok(true)
}

/// Whether the branch `efference/<id>` exists in `project`'s repository: the branch
/// an earlier checkout left behind (docs/worktrees.md).
pub fn branch_exists(project: &Path, id: &str, env: &ChildEnv) -> bool {
    git(
        project,
        env,
        &[
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("refs/heads/{}", branch(id)),
        ],
    )
    .is_ok_and(|output| output.status.success())
}
