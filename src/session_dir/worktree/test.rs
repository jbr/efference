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
    // Compared as text too: `PathBuf` equality ignores a trailing separator, which a
    // reviewer shown the path does not.
    let root = counterpart(project, Path::new("/p/.efference/worktrees/quiet-fox"));
    assert_eq!(root.unwrap().to_str(), Some("/p"));
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
