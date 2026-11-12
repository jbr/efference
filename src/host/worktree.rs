//! The git operations on a session's own worktree (docs/worktrees.md): create it, retire
//! it at archive, re-create it on resume. Where it lives is
//! [`crate::record::session_dir::worktree`].

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use crate::protocol::sessions::RetiredWorktree;

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

impl Added {
    /// The log entry for this checkout; `restored` for a re-creation on resume.
    pub fn into_event(self, restored: bool) -> crate::record::log::WorktreeEvent {
        let checkout = crate::record::log::Checkout {
            base: self.base,
            exit_code: self.exit_code,
            output: self.output,
        };
        crate::record::log::WorktreeEvent {
            path: self.path,
            branch: self.branch,
            kind: if restored {
                crate::record::log::WorktreeEventKind::Restored(checkout)
            } else {
                crate::record::log::WorktreeEventKind::Added(checkout)
            },
        }
    }
}

impl Retired {
    /// The log entry for this outcome; `None` when there was nothing to record.
    pub fn into_event(self, project: &Path, id: &str) -> Option<crate::record::log::WorktreeEvent> {
        use crate::record::log::WorktreeEventKind as Kind;
        let kind = match self {
            Retired::Removed { branch_deleted } => Kind::Removed { branch_deleted },
            Retired::Kept { reason } => Kind::Kept { reason },
            Retired::Failed(error) => Kind::RemoveFailed { error },
            Retired::Absent => return None,
        };
        Some(crate::record::log::WorktreeEvent {
            path: crate::record::session_dir::worktree::path(project, id),
            branch: branch(id),
            kind,
        })
    }

    /// The `worktree` field of an archive response (docs/control-api.md); `None` when
    /// there was no worktree directory.
    pub fn response(&self) -> Option<RetiredWorktree> {
        match self {
            Retired::Removed { branch_deleted } => Some(RetiredWorktree::Removed {
                removed: true,
                branch_deleted: *branch_deleted,
            }),
            Retired::Kept { reason } => Some(RetiredWorktree::Kept {
                kept: reason.clone(),
            }),
            Retired::Failed(error) => Some(RetiredWorktree::Failed {
                failed: error.clone(),
            }),
            Retired::Absent => None,
        }
    }
}

/// The most of git's output kept.
const OUTPUT_CAP: usize = 64 * 1024;

/// A session's branch: `efference/<id>`.
pub fn branch(id: &str) -> String {
    format!("efference/{id}")
}

fn git(
    dir: &Path,
    env: &crate::config::env_table::ChildEnv,
    args: &[&str],
) -> Result<Output, String> {
    let mut cmd = Command::new("git");
    cmd.args(args).current_dir(dir);
    env.apply(&mut cmd);
    unset_repository_env(&mut cmd);
    crate::platform::spawn::std_output(&mut cmd).map_err(|e| format!("could not run git: {e}"))
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
    let mut text = without_overwrites(&String::from_utf8_lossy(&output.stdout));
    text.push_str(&without_overwrites(&String::from_utf8_lossy(
        &output.stderr,
    )));
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

/// Each line as a terminal would leave it: the text after its last carriage return.
/// Git's progress meters (`Updating files:  96% …\r`) redraw one line in place, and
/// kept verbatim they are most of what a checkout prints.
fn without_overwrites(text: &str) -> String {
    text.split_inclusive('\n')
        .map(|line| {
            let (body, newline) = match line.strip_suffix('\n') {
                Some(body) => (body, "\n"),
                None => (line, ""),
            };
            let body = body.strip_suffix('\r').unwrap_or(body);
            let last = body.rsplit('\r').next().unwrap_or(body);
            format!("{last}{newline}")
        })
        .collect()
}

/// The commit `from`'s `HEAD` names, when `from` is inside a git work tree and `HEAD`
/// resolves: the two preconditions of [`add`]. The error says which one failed.
fn head(from: &Path, env: &crate::config::env_table::ChildEnv) -> Result<String, String> {
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
    Ok(String::from_utf8_lossy(&head.stdout).trim().to_string())
}

/// Whether [`add`] could start a worktree from `from`: the creation preview's
/// `worktree` answer (docs/control-api.md). Run with the host's own environment, as
/// the session's is not known until creation.
pub fn can_start(from: &Path) -> bool {
    head(from, &crate::config::env_table::ChildEnv::default()).is_ok()
}

/// Create the session's worktree: `git worktree add -b efference/<id> <path> <base>`,
/// run in `from` (the creator's working directory) with the session's child
/// environment, on the commit `from`'s `HEAD` names (docs/worktrees.md). The
/// checkout is plain, so the project's `post-checkout` hook runs.
///
/// A path that exists, a worktree `project`'s git already registers there, or a
/// branch `efference/<id>` already present refuses the call before git runs:
/// leftovers of an earlier session are routed around, never adopted. Afterward the
/// result must be this tree's checkout — registered at exactly `path`, `HEAD` on
/// the requested branch, git dir usable — or the call fails and leaves whatever it
/// found untouched (docs/worktrees.md). A hook that fails makes git exit nonzero
/// with a coherent checkout in place; that returns `Ok` with the status and output
/// recorded.
pub fn add(
    project: &Path,
    id: &str,
    from: &Path,
    env: &crate::config::env_table::ChildEnv,
) -> Result<Added, String> {
    let base = head(from, env)?;
    let path = crate::record::session_dir::worktree::path(project, id);
    let branch = branch(id);
    if path.symlink_metadata().is_ok() {
        return Err(format!(
            "{} already exists, so it is not this creation's to check out into",
            path.display()
        ));
    }
    if registered(project, &path, env)? {
        return Err(format!(
            "{} is already a registered worktree of {}",
            path.display(),
            project.display()
        ));
    }
    if branch_exists(project, id, env) {
        return Err(format!(
            "the branch {branch} already exists, left by an earlier checkout"
        ));
    }
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
    checked(project, path, branch, base, &added, env)
}

/// Undo [`add`] after a failed boot: remove the worktree, then its branch. Only a
/// checkout this tree's identity still stands behind — [`owned_checkout`] — is
/// removed; a path an external actor repurposed is left and named on the log
/// (docs/worktrees.md). Best effort; a failure is named on the log (`warn`).
pub fn remove(project: &Path, path: &Path, branch: &str, env: &crate::config::env_table::ChildEnv) {
    match owned_checkout(project, path, branch, env) {
        Ok(true) => {}
        Ok(false) => {
            log::warn!(
                "worktree: leaving {} and branch {branch}: the checkout there is not this one's",
                path.display()
            );
            return;
        }
        Err(reading) => {
            log::warn!(
                "worktree: leaving {} and branch {branch}: the checkout cannot be read: {reading}",
                path.display()
            );
            return;
        }
    }
    let path = path.to_string_lossy();
    for args in [
        vec!["worktree", "remove", "--force", &path],
        vec!["branch", "-D", branch],
    ] {
        match git(project, env, &args) {
            Ok(output) if output.status.success() => {}
            Ok(output) => log::warn!(
                "worktree: `git {}` failed: {}",
                args.join(" "),
                combined(&output).trim()
            ),
            Err(e) => log::warn!("worktree: {e}"),
        }
    }
}

/// What archiving did about a session's own worktree.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Retired {
    /// The directory was clean and is gone; the branch went too when another branch
    /// contains its tip.
    Removed { branch_deleted: bool },
    /// Left in place, with the reason.
    Kept { reason: String },
    /// The directory does not exist: nothing to do.
    Absent,
    /// Git failed; the directory is as it was.
    Failed(String),
}

/// The most porcelain lines named in a [`Retired::Kept`] reason.
const SUMMARY_LINES: usize = 5;

/// Archive a session's own worktree (docs/worktrees.md): remove it when
/// `git status --porcelain` there is empty (untracked files count, ignored ones do not),
/// then delete the branch only if another branch contains its tip, since the branch is
/// otherwise the only copy of its commits. A dirty tree, or a detached `HEAD` that no
/// branch contains, is kept untouched.
///
/// `--force` on the removal is for git's refusal to remove a tree holding initialized
/// submodules even when clean; the porcelain check is the safety.
pub fn retire(project: &Path, id: &str, env: &crate::config::env_table::ChildEnv) -> Retired {
    let path = crate::record::session_dir::worktree::path(project, id);
    let branch = branch(id);
    if !path.is_dir() {
        return Retired::Absent;
    }
    match retire_in(project, &path, &branch, env) {
        Ok(retired) => retired,
        Err(error) => Retired::Failed(error),
    }
}

fn retire_in(
    project: &Path,
    path: &Path,
    branch: &str,
    env: &crate::config::env_table::ChildEnv,
) -> Result<Retired, String> {
    // Before any removal: only a checkout this tree's identity stands behind may be
    // removed — a path another repository registered, or a checkout someone moved
    // off the tree's branch, is left untouched with its branch
    // (docs/worktrees.md). A detached `HEAD` passes this gate and is judged by the
    // containment check below, as it has been.
    if !registered(project, path, env)? {
        return Ok(Retired::Kept {
            reason: format!(
                "{} is not a registered worktree of {}",
                path.display(),
                project.display()
            ),
        });
    }
    if let Some(on) = head_branch(path, env)?
        && on != branch
    {
        return Ok(Retired::Kept {
            reason: format!("HEAD is on {on}, not {branch}"),
        });
    }
    let status = git_ok(path, env, &["status", "--porcelain"])?;
    let lines: Vec<&str> = status.lines().filter(|l| !l.trim().is_empty()).collect();
    if !lines.is_empty() {
        let mut named: Vec<&str> = lines.iter().take(SUMMARY_LINES).map(|l| l.trim()).collect();
        let more = format!("and {} more", lines.len().saturating_sub(SUMMARY_LINES));
        if lines.len() > SUMMARY_LINES {
            named.push(&more);
        }
        return Ok(Retired::Kept {
            reason: format!("uncommitted changes: {}", named.join("; ")),
        });
    }
    let on_branch = git(path, env, &["symbolic-ref", "-q", "HEAD"])?
        .status
        .success();
    if !on_branch {
        let holders = git_ok(
            path,
            env,
            &["branch", "--contains", "HEAD", "--format=%(refname:short)"],
        )?;
        if !holders
            .lines()
            .any(|l| !l.starts_with('(') && !l.is_empty())
        {
            return Ok(Retired::Kept {
                reason: "HEAD is detached at a commit no branch contains".into(),
            });
        }
    }
    git_ok(
        project,
        env,
        &["worktree", "remove", "--force", &path.to_string_lossy()],
    )?;
    Ok(Retired::Removed {
        branch_deleted: delete_branch_if_contained(project, branch, env),
    })
}

/// Delete `branch` when some other branch contains its tip. Anything that goes wrong
/// keeps the branch.
fn delete_branch_if_contained(
    project: &Path,
    branch: &str,
    env: &crate::config::env_table::ChildEnv,
) -> bool {
    let Ok(holders) = git_ok(
        project,
        env,
        &["branch", "--contains", branch, "--format=%(refname:short)"],
    ) else {
        return false;
    };
    holders.lines().any(|l| !l.is_empty() && l != branch)
        && git_ok(project, env, &["branch", "-D", branch]).is_ok()
}

/// Stdout of a git command that must succeed; its combined output as the error if not.
fn git_ok(
    dir: &Path,
    env: &crate::config::env_table::ChildEnv,
    args: &[&str],
) -> Result<String, String> {
    let output = git(dir, env, args)?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    } else {
        Err(format!(
            "git {} failed: {}",
            args.join(" "),
            combined(&output).trim()
        ))
    }
}

/// Re-create a managed worktree at its original path, for a resumed session whose
/// working directory sits at or inside it and whose tree [`retire`] removed: from the
/// kept branch when it exists (`git worktree add <path> <branch>`), else a fresh
/// branch at the project's `HEAD`. Run in `project` with the session's child
/// environment and a plain checkout, so `post-checkout` fires as it did at creation.
/// Path and branch are unchanged, so the preamble stays accurate. The tree's own
/// identity names both, not the resuming session's (docs/worktrees.md).
///
/// Callers serialize on the managed root's lock, so a second restore finds the first
/// one's checkout in place: that returns `Ok(None)`, nothing appended. A directory
/// that exists and is not this tree's checkout refuses the call and is left alone
/// (docs/worktrees.md).
pub fn restore(
    project: &Path,
    id: &str,
    env: &crate::config::env_table::ChildEnv,
) -> Result<Option<Added>, String> {
    let path = crate::record::session_dir::worktree::path(project, id);
    let branch = branch(id);
    if path.symlink_metadata().is_ok() {
        return if checkout_is(project, &path, &branch, env).unwrap_or(false) {
            Ok(None)
        } else {
            Err(format!(
                "{} already exists and is not a checkout of {branch}",
                path.display()
            ))
        };
    }
    // A directory removed by hand leaves its registration behind, which `add` refuses.
    git_ok(project, env, &["worktree", "prune"])?;
    let kept = git(
        project,
        env,
        &[
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("refs/heads/{branch}"),
        ],
    )?;
    let path_arg = path.to_string_lossy();
    let (base, added) = if kept.status.success() {
        let base = String::from_utf8_lossy(&kept.stdout).trim().to_string();
        let added = git(project, env, &["worktree", "add", &path_arg, &branch])?;
        (base, added)
    } else {
        let head = git_ok(project, env, &["rev-parse", "HEAD"])?
            .trim()
            .to_string();
        let added = git(
            project,
            env,
            &["worktree", "add", "-b", &branch, &path_arg, "HEAD"],
        )?;
        (head, added)
    };
    checked(project, path, branch, base, &added, env).map(Some)
}

mod checkout;
pub use checkout::branch_exists;
use checkout::{checked, checkout_is, head_branch, owned_checkout, registered};

#[cfg(test)]
mod test;
#[cfg(test)]
pub(crate) mod testing;
