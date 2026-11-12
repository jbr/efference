use super::testing::{commit, git, repository};
use super::*;

fn temp_dir(label: &str) -> PathBuf {
    use std::hash::{BuildHasher, Hasher, RandomState};
    let n = RandomState::new().build_hasher().finish();
    let dir = std::env::temp_dir().join(format!("efference-worktree-{label}-{n:016x}"));
    std::fs::create_dir_all(&dir).unwrap();
    dir.canonicalize().unwrap()
}

#[test]
fn add_makes_a_worktree_on_a_new_branch_at_head_and_remove_undoes_it() {
    let project = temp_dir("add");
    git(&project, &["init", "-q"]);
    std::fs::write(project.join("f.txt"), "hi").unwrap();
    git(&project, &["add", "f.txt"]);
    commit(&project, "one");
    let head = git(&project, &["rev-parse", "HEAD"]);
    let env = crate::config::env_table::ChildEnv::default();

    let added = add(&project, "quiet-fox", &project, &env).unwrap();
    assert_eq!(
        added.path,
        crate::record::session_dir::worktree::path(&project, "quiet-fox")
    );
    assert!(added.path.join("f.txt").is_file());
    assert_eq!(added.branch, "efference/quiet-fox");
    assert_eq!(added.base, head);
    assert_eq!(added.exit_code, Some(0));
    assert_eq!(
        git(&added.path, &["rev-parse", "--abbrev-ref", "HEAD"]),
        "efference/quiet-fox"
    );
    assert_eq!(git(&added.path, &["rev-parse", "HEAD"]), head);

    remove(&project, &added.path, &added.branch, &env);
    assert!(!added.path.exists());
    assert_eq!(
        git(&project, &["branch", "--list", "efference/quiet-fox"]),
        ""
    );
    std::fs::remove_dir_all(&project).unwrap();
}

/// A `GIT_DIR` in the session environment, as in one captured inside a git hook,
/// names another repository; the worktree is still made in the project's.
#[test]
fn add_ignores_a_git_dir_in_the_environment() {
    let project = temp_dir("project");
    let other = temp_dir("other");
    for repo in [&project, &other] {
        repository(repo);
    }
    let env = crate::config::env_table::ChildEnv::default().with_inject(
        [(
            "GIT_DIR".to_string(),
            other.join(".git").to_string_lossy().into_owned(),
        )]
        .into(),
    );

    let added = add(&project, "quiet-fox", &project, &env).unwrap();
    assert!(git(&project, &["worktree", "list"]).contains("quiet-fox"));
    assert!(!git(&other, &["worktree", "list"]).contains("quiet-fox"));
    assert_eq!(git(&other, &["config", "core.bare"]), "false");

    remove(&project, &added.path, &added.branch, &env);
    std::fs::remove_dir_all(&project).unwrap();
    std::fs::remove_dir_all(&other).unwrap();
}

/// A repository with one commit and a session worktree `quiet-fox` on its own branch.
fn project_with_worktree() -> (PathBuf, Added) {
    let project = temp_dir("retire");
    repository(&project);
    let env = crate::config::env_table::ChildEnv::default();
    let added = add(&project, "quiet-fox", &project, &env).unwrap();
    (project, added)
}

fn branches(project: &Path) -> String {
    git(project, &["branch", "--format=%(refname:short)"])
}

#[test]
fn retire_removes_a_clean_worktree_and_a_branch_with_no_unique_commits() {
    let (project, added) = project_with_worktree();
    let env = crate::config::env_table::ChildEnv::default();
    // An ignored file does not make the tree dirty.
    std::fs::write(project.join(".git/info/exclude"), "target/\n").unwrap();
    std::fs::create_dir(added.path.join("target")).unwrap();
    std::fs::write(added.path.join("target/artifact"), "x").unwrap();

    assert_eq!(
        retire(&project, "quiet-fox", &env),
        Retired::Removed {
            branch_deleted: true
        }
    );
    assert!(!added.path.exists());
    assert!(!branches(&project).contains("efference/quiet-fox"));
    assert_eq!(retire(&project, "quiet-fox", &env), Retired::Absent);
    std::fs::remove_dir_all(&project).unwrap();
}

#[test]
fn retire_keeps_a_branch_holding_a_unique_commit() {
    let (project, added) = project_with_worktree();
    let env = crate::config::env_table::ChildEnv::default();
    commit(&added.path, "only here");

    assert_eq!(
        retire(&project, "quiet-fox", &env),
        Retired::Removed {
            branch_deleted: false
        }
    );
    assert!(!added.path.exists());
    assert!(branches(&project).contains("efference/quiet-fox"));
    std::fs::remove_dir_all(&project).unwrap();
}

#[test]
fn retire_keeps_a_dirty_worktree_untouched() {
    let (project, added) = project_with_worktree();
    let env = crate::config::env_table::ChildEnv::default();
    std::fs::write(added.path.join("scratch.txt"), "work").unwrap();

    let Retired::Kept { reason } = retire(&project, "quiet-fox", &env) else {
        panic!("a dirty worktree is kept");
    };
    assert!(reason.contains("scratch.txt"), "{reason}");
    assert!(added.path.join("scratch.txt").is_file());
    assert!(branches(&project).contains("efference/quiet-fox"));
    std::fs::remove_dir_all(&project).unwrap();
}

#[test]
fn restore_re_adds_from_the_kept_branch() {
    let (project, added) = project_with_worktree();
    let env = crate::config::env_table::ChildEnv::default();
    commit(&added.path, "only here");
    let tip = git(&added.path, &["rev-parse", "HEAD"]);
    retire(&project, "quiet-fox", &env);
    assert!(!added.path.exists());

    let restored = restore(&project, "quiet-fox", &env)
        .unwrap()
        .expect("restored");
    assert_eq!(restored.path, added.path);
    assert_eq!(restored.branch, "efference/quiet-fox");
    assert_eq!(restored.base, tip);
    assert_eq!(git(&restored.path, &["rev-parse", "HEAD"]), tip);
    assert_eq!(
        git(&restored.path, &["rev-parse", "--abbrev-ref", "HEAD"]),
        "efference/quiet-fox"
    );
    std::fs::remove_dir_all(&project).unwrap();
}

#[test]
fn restore_starts_a_fresh_branch_when_the_branch_is_gone() {
    let (project, added) = project_with_worktree();
    let env = crate::config::env_table::ChildEnv::default();
    let head = git(&project, &["rev-parse", "HEAD"]);
    retire(&project, "quiet-fox", &env);
    assert!(!branches(&project).contains("efference/quiet-fox"));

    let restored = restore(&project, "quiet-fox", &env)
        .unwrap()
        .expect("restored");
    assert_eq!(restored.path, added.path);
    assert_eq!(restored.base, head);
    assert_eq!(
        git(&restored.path, &["rev-parse", "--abbrev-ref", "HEAD"]),
        "efference/quiet-fox"
    );
    std::fs::remove_dir_all(&project).unwrap();
}

#[test]
fn restore_recovers_a_directory_deleted_by_hand() {
    let (project, added) = project_with_worktree();
    let env = crate::config::env_table::ChildEnv::default();
    std::fs::remove_dir_all(&added.path).unwrap();

    let restored = restore(&project, "quiet-fox", &env)
        .unwrap()
        .expect("restored");
    assert!(restored.path.is_dir());
    std::fs::remove_dir_all(&project).unwrap();
}

#[test]
fn add_outside_a_git_work_tree_is_refused() {
    let dir = temp_dir("plain");
    let env = crate::config::env_table::ChildEnv::default();
    let error = add(&dir, "quiet-fox", &dir, &env).unwrap_err();
    assert!(error.contains("git work tree"), "{error}");
    assert!(!crate::record::session_dir::worktree::path(&dir, "quiet-fox").exists());
    std::fs::remove_dir_all(&dir).unwrap();
}

/// A progress meter's redraws keep only their last state, the line a terminal would
/// show; lines without a carriage return pass through.
#[test]
fn progress_redraws_keep_their_last_state() {
    let git = "HEAD is now at b5a984a7 smoke\nPreparing worktree\nUpdating files:  96% (1438/1485)\rUpdating files: 100% (1485/1485)\rUpdating files: 100% (1485/1485), done.\n";
    assert_eq!(
        without_overwrites(git),
        "HEAD is now at b5a984a7 smoke\nPreparing worktree\nUpdating files: 100% (1485/1485), done.\n"
    );
    assert_eq!(without_overwrites("done.\r\n"), "done.\n");
    assert_eq!(without_overwrites("no newline"), "no newline");
}

/// What an archive returns about the worktree, in the three shapes it takes
/// (docs/control-api.md): the host builds each from the outcome and a client reads the
/// key identifying which it was. A git failure is not reachable from a route test, so its
/// bytes are pinned here.
#[test]
fn a_retired_worktree_answers_with_the_key_that_names_it() {
    use crate::protocol::sessions::RetiredWorktree;
    let answer = |retired: Retired| serde_json::to_string(&retired.response()).unwrap();
    assert_eq!(
        answer(Retired::Removed {
            branch_deleted: true
        }),
        r#"{"removed":true,"branch_deleted":true}"#
    );
    assert_eq!(
        answer(Retired::Kept {
            reason: "uncommitted changes: ?? notes.txt".into()
        }),
        r#"{"kept":"uncommitted changes: ?? notes.txt"}"#
    );
    assert_eq!(
        answer(Retired::Failed("could not run git".into())),
        r#"{"failed":"could not run git"}"#
    );
    assert_eq!(answer(Retired::Absent), "null");
    for retired in [
        RetiredWorktree::Removed {
            removed: true,
            branch_deleted: false,
        },
        RetiredWorktree::Kept { kept: "k".into() },
        RetiredWorktree::Failed { failed: "f".into() },
    ] {
        let text = serde_json::to_string(&retired).unwrap();
        assert_eq!(
            serde_json::from_str::<RetiredWorktree>(&text).unwrap(),
            retired
        );
    }
}

/// A directory left at the checkout path is routed around, never adopted: the call
/// refuses before git runs and leaves what it found alone (docs/worktrees.md).
#[test]
fn add_refuses_a_directory_left_at_the_checkout_path() {
    let project = temp_dir("stale-dir");
    git(&project, &["init", "-q"]);
    std::fs::write(project.join("f.txt"), "hi").unwrap();
    git(&project, &["add", "f.txt"]);
    commit(&project, "one");
    let env = crate::config::env_table::ChildEnv::default();
    let path = crate::record::session_dir::worktree::path(&project, "quiet-fox");
    std::fs::create_dir_all(path.join("tmp")).unwrap();
    std::fs::write(path.join("tmp/checklist.md"), "mine").unwrap();

    let error = add(&project, "quiet-fox", &project, &env).unwrap_err();
    assert!(error.contains("already exists"), "{error}");
    assert!(path.join("tmp/checklist.md").is_file(), "left as it was");
    assert!(
        !branch_exists(&project, "quiet-fox", &env),
        "no branch made"
    );
    std::fs::remove_dir_all(&project).unwrap();
}

/// A branch left by an earlier checkout takes the name out of allocation and refuses
/// the add with it (docs/worktrees.md).
#[test]
fn add_refuses_a_branch_left_by_an_earlier_checkout() {
    let project = temp_dir("stale-branch");
    repository(&project);
    let env = crate::config::env_table::ChildEnv::default();
    git(&project, &["branch", "efference/quiet-fox"]);

    let error = add(&project, "quiet-fox", &project, &env).unwrap_err();
    assert!(
        error.contains("efference/quiet-fox already exists"),
        "{error}"
    );
    assert!(!crate::record::session_dir::worktree::path(&project, "quiet-fox").exists());
    std::fs::remove_dir_all(&project).unwrap();
}

/// A registration left behind by a directory deleted by hand still refuses the add:
/// the path is registered, so it is not this creation's (docs/worktrees.md).
#[test]
fn add_refuses_a_path_registered_without_its_directory() {
    let (project, added) = project_with_worktree();
    let env = crate::config::env_table::ChildEnv::default();
    std::fs::remove_dir_all(&added.path).unwrap();

    let error = add(&project, "quiet-fox", &project, &env).unwrap_err();
    assert!(error.contains("registered worktree"), "{error}");
    std::fs::remove_dir_all(&project).unwrap();
}

/// A `post-checkout` hook that fails makes git exit nonzero with a coherent checkout
/// in place: the outcome is recorded, not refused (docs/worktrees.md).
#[test]
fn a_failed_hook_keeps_the_checkout_with_its_status() {
    let project = temp_dir("hook-fail");
    git(&project, &["init", "-q"]);
    std::fs::write(project.join("f.txt"), "hi").unwrap();
    git(&project, &["add", "f.txt"]);
    commit(&project, "one");
    let hook = project.join(".git/hooks/post-checkout");
    std::fs::write(&hook, "#!/bin/sh\nexit 3\n").unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();
    let env = crate::config::env_table::ChildEnv::default();

    let added = add(&project, "quiet-fox", &project, &env).unwrap();
    assert_eq!(added.exit_code, Some(3), "the hook's status is its own");
    assert!(added.path.is_dir());
    assert_eq!(
        git(&added.path, &["rev-parse", "--abbrev-ref", "HEAD"]),
        "efference/quiet-fox"
    );
    std::fs::remove_dir_all(&project).unwrap();
}

/// A checkout someone moved off the tree's branch is not the tree's to remove: retire
/// keeps it, with its branch (docs/worktrees.md).
#[test]
fn retire_keeps_a_checkout_moved_off_the_tree_branch() {
    let (project, added) = project_with_worktree();
    let env = crate::config::env_table::ChildEnv::default();
    git(&added.path, &["checkout", "-q", "-b", "repurposed"]);

    let Retired::Kept { reason } = retire(&project, "quiet-fox", &env) else {
        panic!("the checkout is kept");
    };
    assert!(reason.contains("repurposed"), "{reason}");
    assert!(added.path.is_dir(), "kept");
    assert!(branch_exists(&project, "quiet-fox", &env), "branch kept");
    std::fs::remove_dir_all(&project).unwrap();
}

/// The rollback's removal passes the same gate: a checkout that is no longer this
/// mint's is left, with its branch (docs/worktrees.md).
#[test]
fn remove_leaves_a_checkout_that_is_no_longer_its_own() {
    let (project, added) = project_with_worktree();
    let env = crate::config::env_table::ChildEnv::default();
    git(&added.path, &["checkout", "-q", "-b", "repurposed"]);

    remove(&project, &added.path, &added.branch, &env);
    assert!(added.path.is_dir(), "kept");
    assert!(branch_exists(&project, "quiet-fox", &env), "branch kept");
    std::fs::remove_dir_all(&project).unwrap();
}

/// A restore that finds the tree's own checkout in place appends nothing and does
/// nothing: the second resume of one removed tree (docs/worktrees.md).
#[test]
fn restore_finds_a_checkout_already_in_place() {
    let (project, added) = project_with_worktree();
    let env = crate::config::env_table::ChildEnv::default();

    let restored = restore(&project, "quiet-fox", &env).unwrap();
    assert!(restored.is_none(), "nothing to re-create");
    assert!(added.path.is_dir());
    std::fs::remove_dir_all(&project).unwrap();
}

/// A restore that finds a directory that is not the tree's checkout refuses it and
/// leaves it alone (docs/worktrees.md).
#[test]
fn restore_refuses_a_directory_that_is_not_the_trees_checkout() {
    let project = temp_dir("restore-foreign");
    git(&project, &["init", "-q"]);
    std::fs::write(project.join("f.txt"), "hi").unwrap();
    git(&project, &["add", "f.txt"]);
    commit(&project, "one");
    let env = crate::config::env_table::ChildEnv::default();
    let path = crate::record::session_dir::worktree::path(&project, "quiet-fox");
    std::fs::create_dir_all(&path).unwrap();

    let error = restore(&project, "quiet-fox", &env).unwrap_err();
    assert!(
        error.contains("is not a checkout of efference/quiet-fox"),
        "{error}"
    );
    assert!(path.is_dir(), "left as it was");
    std::fs::remove_dir_all(&project).unwrap();
}
