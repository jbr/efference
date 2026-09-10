use super::*;

#[test]
fn storage_can_be_outside_the_working_directory() {
    let paths = SessionPaths::new("/projects/one".into(), "/state/sessions/one".into()).unwrap();
    assert_eq!(paths.cwd(), Path::new("/projects/one"));
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
    let one = SessionPaths::new(
        "/projects/one".into(),
        "/projects/one/.efference/run".into(),
    )
    .unwrap();
    let two = SessionPaths::new("/projects/two".into(), one.storage().into()).unwrap();
    assert_eq!(
        one.attachment_display(),
        Path::new(".efference/run/attachments")
    );
    assert_eq!(two.attachment_display(), one.attachments());
}

#[test]
fn relative_boot_paths_are_rejected() {
    for (cwd, storage) in [("relative", "/state"), ("/project", "relative")] {
        assert_eq!(
            SessionPaths::new(cwd.into(), storage.into())
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidInput
        );
    }
}
