use super::*;

fn at(dir: &str, storage: &str) -> io::Result<SessionPaths> {
    SessionPaths::new(Directories::at(Path::new(dir)), storage.into())
}

#[test]
fn storage_can_be_outside_the_working_directory() {
    let paths = at("/projects/one", "/state/sessions/one").unwrap();
    assert_eq!(paths.working(), Path::new("/projects/one"));
    assert_eq!(
        paths.usage_log(),
        Path::new("/state/sessions/one/usage.jsonl")
    );
    assert_eq!(
        paths.attachments(),
        Path::new("/state/sessions/one/attachments")
    );
    assert_eq!(paths.attachment_display(), paths.attachments());
}

#[test]
fn attachment_labels_use_the_session_root() {
    let one = at("/projects/one", "/projects/one/.efference/run").unwrap();
    let two = at("/projects/two", "/projects/one/.efference/run").unwrap();
    assert_eq!(
        one.attachment_display(),
        Path::new(".efference/run/attachments")
    );
    assert_eq!(two.attachment_display(), one.attachments());
}

#[test]
fn a_worktree_session_addresses_its_attachments_absolutely() {
    let paths = SessionPaths::new(
        Directories::new(
            "/projects/one".into(),
            "/projects/one/.efference/worktrees/run".into(),
        ),
        "/projects/one/.efference/run".into(),
    )
    .unwrap();
    assert_eq!(paths.project(), Path::new("/projects/one"));
    assert_eq!(
        paths.working(),
        Path::new("/projects/one/.efference/worktrees/run")
    );
    assert_eq!(paths.attachment_display(), paths.attachments());
}

#[test]
fn relative_boot_paths_are_rejected() {
    for (cwd, storage) in [("relative", "/state"), ("/project", "relative")] {
        assert_eq!(
            at(cwd, storage).unwrap_err().kind(),
            io::ErrorKind::InvalidInput
        );
    }
}
