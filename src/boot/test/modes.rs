//! Owner-only modes on a project's `.efference/` and what a session writes into it
//! (docs/state-store.md).

use super::{Root, base_env, codex_registry, drop_boot, request};
use crate::boot::ResumeRequest;
use crate::boot::{CONFIG_NAME, mint_session_dir, prepare, resume};
use crate::record::session_dir::{LOG_NAME, SESSION_BASE};
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

fn mode(path: &Path) -> u32 {
    std::fs::metadata(path).unwrap().permissions().mode() & 0o777
}

fn set_mode(path: &Path, mode: u32) {
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
}

/// A created session: the project's `.efference/` and the session directory are
/// `0700`, the log and the config snapshot `0600`. The process umask is left alone (it
/// is process-wide, and the tests run in parallel); under the usual `022` a mode the
/// code left to the umask would read `0755` or `0644` here.
#[tokio::test]
async fn a_created_session_is_owner_only() {
    let root = Root::new("modes-create");
    let boot = prepare(request(root.path(), "private", None))
        .await
        .unwrap();
    let dir = root.session_dir("private");
    assert_eq!(mode(&root.path().join(SESSION_BASE)), 0o700, ".efference/");
    assert_eq!(mode(&dir), 0o700, "the session directory");
    assert_eq!(mode(&dir.join(LOG_NAME)), 0o600, "the log");
    assert_eq!(mode(&dir.join(CONFIG_NAME)), 0o600, "the config snapshot");
    drop_boot(boot);
}

/// The files a running session adds under its directory — the usage record, an
/// attachment — are `0600`.
#[test]
fn files_added_to_a_session_directory_are_owner_only() {
    let root = Root::new("modes-files");
    let dir = root.session_dir("files");
    mint_session_dir(&dir).unwrap();

    let attached =
        crate::session::attachments::store(&dir.join("attachments"), "note.txt", b"hi").unwrap();
    assert_eq!(mode(&attached), 0o600, "an attachment");

    let usage = dir.join(crate::session::USAGE_NAME);
    let log = crate::record::persist::Log::create(dir.join(LOG_NAME), None).unwrap();
    let (session, _turns) = crate::session::Session::new(root.path().to_path_buf(), log);
    session.attach_usage_log(&usage);
    assert_eq!(mode(&usage), 0o600, "the usage record");
}

/// A `.efference/` an earlier run left at `0755` is tightened when a session is minted
/// in it; a stricter one is left as it is (the one-way rule of `daemon::permissions`).
#[test]
fn minting_a_session_tightens_an_open_project_base() {
    let root = Root::new("modes-mint");
    let base = root.path().join(SESSION_BASE);
    std::fs::create_dir(&base).unwrap();
    set_mode(&base, 0o755);
    mint_session_dir(&root.session_dir("a")).unwrap();
    assert_eq!(mode(&base), 0o700);

    set_mode(&base, 0o500);
    crate::daemon::permissions::tighten_project_base(&base);
    assert_eq!(mode(&base), 0o500, "a stricter mode stays");
    set_mode(&base, 0o700);
}

/// Resuming a session in a project whose `.efference/` is `0755` tightens it.
#[tokio::test]
async fn resuming_a_session_tightens_an_open_project_base() {
    let root = Root::new("modes-resume");
    drop_boot(prepare(request(root.path(), "again", None)).await.unwrap());
    let base = root.path().join(SESSION_BASE);
    set_mode(&base, 0o755);

    let registry = codex_registry(root.path(), "again");
    let (boot, _options) = resume(ResumeRequest {
        session_dir: root.session_dir("again"),
        base_env: base_env(),
        inference: &registry,
        reauth: Vec::new(),
        authorization: Default::default(),
        switch: Default::default(),
        host: None,
    })
    .await
    .expect("the session resumes");
    assert_eq!(mode(&base), 0o700);
    drop_boot(boot);
}
