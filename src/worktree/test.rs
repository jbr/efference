use super::*;

#[test]
fn a_worktree_lives_under_the_session_base() {
    assert_eq!(
        path(Path::new("/p"), "quiet-fox"),
        PathBuf::from("/p/.efference/worktrees/quiet-fox")
    );
}

#[test]
fn a_path_in_a_worktree_stands_for_the_same_path_in_the_project() {
    let project = Path::new("/p");
    assert_eq!(
        counterpart(
            project,
            Path::new("/p/.efference/worktrees/quiet-fox/src/lib.rs")
        ),
        Some(PathBuf::from("/p/src/lib.rs"))
    );
    assert_eq!(
        counterpart(project, Path::new("/p/.efference/worktrees/quiet-fox")),
        Some(PathBuf::from("/p"))
    );
}

#[test]
fn paths_outside_every_worktree_have_no_counterpart() {
    let project = Path::new("/p");
    for path in [
        "/p/src/lib.rs",
        "/p/.efference/quiet-fox/log.jsonl",
        "/p/.efference/worktrees",
        "/elsewhere/.efference/worktrees/quiet-fox/src",
    ] {
        assert_eq!(counterpart(project, Path::new(path)), None, "{path}");
    }
}

#[test]
fn a_directory_in_a_worktree_belongs_to_its_project() {
    for dir in [
        "/p/.efference/worktrees/quiet-fox",
        "/p/.efference/worktrees/quiet-fox/src/bin",
    ] {
        assert_eq!(
            project_of(Path::new(dir)),
            Some(PathBuf::from("/p")),
            "{dir}"
        );
    }
}

/// Run `git` in `dir` through the spawn lock, asserting it succeeds; its stdout.
fn git_in(dir: &Path, args: &[&str]) -> String {
    let mut cmd = std::process::Command::new("git");
    cmd.args(args).current_dir(dir);
    unset_repository_env(&mut cmd);
    let output = crate::spawn::std_output(&mut cmd).unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

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
    git_in(&project, &["init", "-q"]);
    std::fs::write(project.join("f.txt"), "hi").unwrap();
    git_in(&project, &["add", "f.txt"]);
    git_in(
        &project,
        &[
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "commit",
            "-q",
            "-m",
            "one",
        ],
    );
    let head = git_in(&project, &["rev-parse", "HEAD"]);
    let env = crate::env_table::ChildEnv::default();

    let added = add(&project, "quiet-fox", &project, &env).unwrap();
    assert_eq!(added.path, path(&project, "quiet-fox"));
    assert!(added.path.join("f.txt").is_file());
    assert_eq!(added.branch, "efference/quiet-fox");
    assert_eq!(added.base, head);
    assert_eq!(added.exit_code, Some(0));
    assert_eq!(
        git_in(&added.path, &["rev-parse", "--abbrev-ref", "HEAD"]),
        "efference/quiet-fox"
    );
    assert_eq!(git_in(&added.path, &["rev-parse", "HEAD"]), head);

    remove(&project, &added.path, &added.branch, &env);
    assert!(!added.path.exists());
    assert_eq!(
        git_in(&project, &["branch", "--list", "efference/quiet-fox"]),
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
        git_in(repo, &["init", "-q"]);
        git_in(
            repo,
            &[
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@t",
                "commit",
                "-q",
                "--allow-empty",
                "-m",
                "one",
            ],
        );
    }
    let env = crate::env_table::ChildEnv::default().with_inject(
        [(
            "GIT_DIR".to_string(),
            other.join(".git").to_string_lossy().into_owned(),
        )]
        .into(),
    );

    let added = add(&project, "quiet-fox", &project, &env).unwrap();
    assert!(git_in(&project, &["worktree", "list"]).contains("quiet-fox"));
    assert!(!git_in(&other, &["worktree", "list"]).contains("quiet-fox"));
    assert_eq!(git_in(&other, &["config", "core.bare"]), "false");

    remove(&project, &added.path, &added.branch, &env);
    std::fs::remove_dir_all(&project).unwrap();
    std::fs::remove_dir_all(&other).unwrap();
}

#[test]
fn add_outside_a_git_work_tree_is_refused() {
    let dir = temp_dir("plain");
    let env = crate::env_table::ChildEnv::default();
    let error = add(&dir, "quiet-fox", &dir, &env).unwrap_err();
    assert!(error.contains("git work tree"), "{error}");
    assert!(!path(&dir, "quiet-fox").exists());
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn a_directory_in_no_worktree_belongs_to_none() {
    for dir in [
        "/p",
        "/p/src",
        "/p/.efference/worktrees",
        "/p/.efference/quiet-fox",
    ] {
        assert_eq!(project_of(Path::new(dir)), None, "{dir}");
    }
}
