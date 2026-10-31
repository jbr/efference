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
    let env = crate::env_table::ChildEnv::default();

    let added = add(&project, "quiet-fox", &project, &env).unwrap();
    assert_eq!(
        added.path,
        crate::session_dir::worktree::path(&project, "quiet-fox")
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

/// A `GIT_DIR` in the session environment, as one captured inside a git hook carries,
/// names another repository; the worktree is still made in the project's.
#[test]
fn add_ignores_a_git_dir_in_the_environment() {
    let project = temp_dir("project");
    let other = temp_dir("other");
    for repo in [&project, &other] {
        repository(repo);
    }
    let env = crate::env_table::ChildEnv::default().with_inject(
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
    let env = crate::env_table::ChildEnv::default();
    let added = add(&project, "quiet-fox", &project, &env).unwrap();
    (project, added)
}

fn branches(project: &Path) -> String {
    git(project, &["branch", "--format=%(refname:short)"])
}

#[test]
fn retire_removes_a_clean_worktree_and_a_branch_with_no_unique_commits() {
    let (project, added) = project_with_worktree();
    let env = crate::env_table::ChildEnv::default();
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
    let env = crate::env_table::ChildEnv::default();
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
    let env = crate::env_table::ChildEnv::default();
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
    let env = crate::env_table::ChildEnv::default();
    commit(&added.path, "only here");
    let tip = git(&added.path, &["rev-parse", "HEAD"]);
    retire(&project, "quiet-fox", &env);
    assert!(!added.path.exists());

    let restored = restore(&project, "quiet-fox", &env).unwrap();
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
    let env = crate::env_table::ChildEnv::default();
    let head = git(&project, &["rev-parse", "HEAD"]);
    retire(&project, "quiet-fox", &env);
    assert!(!branches(&project).contains("efference/quiet-fox"));

    let restored = restore(&project, "quiet-fox", &env).unwrap();
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
    let env = crate::env_table::ChildEnv::default();
    std::fs::remove_dir_all(&added.path).unwrap();

    let restored = restore(&project, "quiet-fox", &env).unwrap();
    assert!(restored.path.is_dir());
    std::fs::remove_dir_all(&project).unwrap();
}

#[test]
fn add_outside_a_git_work_tree_is_refused() {
    let dir = temp_dir("plain");
    let env = crate::env_table::ChildEnv::default();
    let error = add(&dir, "quiet-fox", &dir, &env).unwrap_err();
    assert!(error.contains("git work tree"), "{error}");
    assert!(!crate::session_dir::worktree::path(&dir, "quiet-fox").exists());
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

/// What an archive answers with about the worktree, in the three shapes it takes
/// (docs/control-api.md): the host builds each from the outcome and a client reads the
/// key that names which it was. A git failure is not reachable from a route test, so its
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
