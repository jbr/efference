//! Where a session's git worktree lives, and the project path a worktree path stands
//! for (docs/worktrees.md). The two decisions about worktree placement and access are
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
/// (docs/worktrees.md). Lexical, like the rest of path classification: `path` must
/// already be absolute and normalized.
pub fn counterpart(project: &Path, path: &Path) -> Option<PathBuf> {
    let worktrees = project.join(crate::daemon::SESSION_BASE).join(WORKTREES);
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

impl Added {
    /// The log entry for this checkout; `restored` for a re-creation on resume.
    pub fn into_event(self, restored: bool) -> crate::log::WorktreeEvent {
        let checkout = crate::log::Checkout {
            base: self.base,
            exit_code: self.exit_code,
            output: self.output,
        };
        crate::log::WorktreeEvent {
            path: self.path,
            branch: self.branch,
            kind: if restored {
                crate::log::WorktreeEventKind::Restored(checkout)
            } else {
                crate::log::WorktreeEventKind::Added(checkout)
            },
        }
    }
}

impl Retired {
    /// The log entry for this outcome; `None` when there was nothing to record.
    pub fn into_event(self, project: &Path, id: &str) -> Option<crate::log::WorktreeEvent> {
        use crate::log::WorktreeEventKind as Kind;
        let kind = match self {
            Retired::Removed { branch_deleted } => Kind::Removed { branch_deleted },
            Retired::Kept { reason } => Kind::Kept { reason },
            Retired::Failed(error) => Kind::RemoveFailed { error },
            Retired::Absent => return None,
        };
        Some(crate::log::WorktreeEvent {
            path: path(project, id),
            branch: branch(id),
            kind,
        })
    }

    /// The `worktree` field of an archive response (docs/control-api.md); `None` when
    /// there was no worktree directory.
    pub fn response(&self) -> Option<serde_json::Value> {
        match self {
            Retired::Removed { branch_deleted } => {
                Some(serde_json::json!({ "removed": true, "branch_deleted": branch_deleted }))
            }
            Retired::Kept { reason } => Some(serde_json::json!({ "kept": reason })),
            Retired::Failed(error) => Some(serde_json::json!({ "failed": error })),
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

/// Create the session's worktree: `git worktree add -b efference/<id> <path> <base>`,
/// run in `from` (the creator's working directory) with the session's child
/// environment, on the commit `from`'s `HEAD` names (docs/worktrees.md). The
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
/// a failure is named on the log (`warn`).
pub fn remove(project: &Path, path: &Path, branch: &str, env: &crate::env_table::ChildEnv) {
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
pub fn retire(project: &Path, id: &str, env: &crate::env_table::ChildEnv) -> Retired {
    let path = path(project, id);
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
    env: &crate::env_table::ChildEnv,
) -> Result<Retired, String> {
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
    env: &crate::env_table::ChildEnv,
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
fn git_ok(dir: &Path, env: &crate::env_table::ChildEnv, args: &[&str]) -> Result<String, String> {
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

/// Re-create a session's own worktree at its original path, for a resumed session whose
/// directory [`retire`] removed: from the kept branch when it exists (`git worktree add
/// <path> <branch>`), else a fresh branch at the project's `HEAD`. Run in `project` with
/// the session's child environment and a plain checkout, so `post-checkout` fires as it
/// did at creation. Path and branch are unchanged, so the preamble stays accurate.
pub fn restore(
    project: &Path,
    id: &str,
    env: &crate::env_table::ChildEnv,
) -> Result<Added, String> {
    let path = path(project, id);
    let branch = branch(id);
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

#[cfg(test)]
mod test;
#[cfg(test)]
pub(crate) mod testing;
