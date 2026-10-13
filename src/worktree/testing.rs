//! Git for tests that build scratch repositories: one `git` that ignores the
//! developer's own configuration, so a global `commit.gpgsign` (a signing agent that
//! prompts, or refuses) or a hook path cannot reach a test's commit.

use std::path::Path;
use std::process::Command;

/// Run `git` in `dir` through the spawn lock with no global or system config and a
/// fixed identity, asserting it succeeds; its trimmed stdout.
pub(crate) fn git(dir: &Path, args: &[&str]) -> String {
    let mut cmd = Command::new("git");
    cmd.args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@t")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@t");
    super::unset_repository_env(&mut cmd);
    let output = crate::spawn::std_output(&mut cmd).unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

/// Commit whatever is staged in `dir`, or nothing, as `message`.
pub(crate) fn commit(dir: &Path, message: &str) {
    git(dir, &["commit", "-q", "--allow-empty", "-m", message]);
}

/// `git init` in `dir` and one empty commit.
pub(crate) fn repository(dir: &Path) {
    git(dir, &["init", "-q"]);
    commit(dir, "one");
}
