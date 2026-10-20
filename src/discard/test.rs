//! The boot sweep removes empty logs and nothing else, against temporary roots.

use super::*;
use crate::log::Payload;
use crate::session_ids::SessionIds;
use std::time::Duration;

struct Dir(PathBuf);

impl Dir {
    fn new() -> Self {
        use std::hash::{BuildHasher, Hasher, RandomState};
        let nonce = RandomState::new().build_hasher().finish();
        let dir = std::env::temp_dir().join(format!("efference-discard-{nonce:016x}"));
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }
}

impl Drop for Dir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A session directory under `<root>/.efference/<id>` whose log holds an
/// `Archived` entry and, with `with_turn`, a `User` one.
fn stored(root: &Path, id: &str, with_turn: bool) -> PathBuf {
    let dir = root.join(".efference").join(id);
    std::fs::create_dir_all(&dir).unwrap();
    let mut log = crate::persist::Log::create(dir.join(crate::boot::LOG_NAME), None).unwrap();
    log.append(0, Payload::archived(None)).unwrap();
    if with_turn {
        log.append(1, Payload::User(vec![])).unwrap();
    }
    log.close().unwrap();
    dir
}

#[test]
fn the_boot_sweep_removes_only_empty_logs() {
    let temp = Dir::new();
    let root = temp.0.join("one");
    let register = SessionIds::at(temp.0.join("state.db"));
    let empty = stored(&root, "left-over", false);
    let kept = stored(&root, "has-turn", true);
    let corrupt = root.join(".efference/garbled");
    std::fs::create_dir_all(&corrupt).unwrap();
    std::fs::write(corrupt.join(crate::boot::LOG_NAME), "not json\n").unwrap();
    let outside = temp.0.join("keep.txt");
    std::fs::write(&outside, "x").unwrap();
    register.reserve("left-over", &root).unwrap();

    let later = SystemTime::now() + Duration::from_secs(60);
    let removed = sweep(std::slice::from_ref(&root), later, &register);

    assert_eq!(removed, vec!["left-over".to_string()]);
    assert!(!empty.exists());
    assert!(kept.exists());
    assert!(corrupt.exists(), "a log that does not parse is left alone");
    assert!(outside.exists());
    assert_eq!(register.holder("left-over"), None);

    // A log written since the host booted may belong to a session it runs.
    let fresh = stored(&root, "fresh", false);
    let earlier = SystemTime::now() - Duration::from_secs(3600);
    assert!(sweep(&[root], earlier, &register).is_empty());
    assert!(fresh.exists());
}

#[test]
fn remove_refuses_a_symlinked_session_directory() {
    let temp = Dir::new();
    let target = temp.0.join("target");
    std::fs::create_dir_all(&target).unwrap();
    let base = temp.0.join("root/.efference");
    std::fs::create_dir_all(&base).unwrap();
    std::os::unix::fs::symlink(&target, base.join("linked")).unwrap();

    let ids = SessionIds::disabled();
    assert!(remove(&temp.0.join("root"), "linked", &ids).is_err());
    assert!(target.exists());
}
