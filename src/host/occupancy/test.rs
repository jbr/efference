use super::*;

use crate::host::starting::{Starting, StartingParts};

fn starting(id: &str, working: &Path) -> Arc<Starting> {
    Starting::new(StartingParts {
        id: id.to_string(),
        directories: crate::config::policy::Directories::new(
            working.to_path_buf(),
            working.to_path_buf(),
        ),
        resume: false,
        created_by: None,
        detached: true,
    })
}

#[test]
fn a_starting_session_occupies_the_root_it_works_in() {
    let root = Path::new("/p/.efference/worktrees/quiet-fox");
    let project = Path::new("/p");
    let mut sessions = Sessions::default();
    assert!(!remains(&sessions, project, root));
    sessions.insert_starting(&starting("tidy-moss", &root.join("src")));
    assert!(remains(&sessions, project, root), "nested working counts");
}

#[test]
fn a_session_working_elsewhere_occupies_nothing_here() {
    let root = Path::new("/p/.efference/worktrees/quiet-fox");
    let project = Path::new("/p");
    let mut sessions = Sessions::default();
    sessions.insert_starting(&starting("tidy-moss", Path::new("/p/src")));
    // Not the root, and not beneath it: a sibling name with the root as a prefix
    // is a different tree.
    sessions.insert_starting(&starting(
        "warm-fjord",
        Path::new("/p/.efference/worktrees/quiet-foxglove"),
    ));
    assert!(!remains(&sessions, project, root));
}
