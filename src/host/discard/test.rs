//! Removing a session directory refuses anything that is not a real directory.

use super::*;
use crate::host::session_ids::SessionIds;
use std::path::PathBuf;

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
