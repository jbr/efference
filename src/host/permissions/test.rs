//! The boot tightening (docs/state-store.md): loose modes tightened, stricter
//! modes kept, absent files skipped — nothing created, nothing failed for
//! being missing.

use super::*;
use std::hash::{BuildHasher, Hasher, RandomState};
use std::os::unix::fs::PermissionsExt;

/// A scratch directory that removes itself when the test ends, however it
/// ends — a panic mid-test must not leave the dir behind.
struct ScratchDir(std::path::PathBuf);

impl std::ops::Deref for ScratchDir {
    type Target = Path;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

/// For the `AsRef<Path>` call sites (`set_permissions` and friends), where
/// deref coercion does not reach a generic.
impl AsRef<Path> for ScratchDir {
    fn as_ref(&self) -> &Path {
        &self.0
    }
}

impl Drop for ScratchDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn scratch() -> ScratchDir {
    let n = RandomState::new().build_hasher().finish();
    let dir = std::env::temp_dir().join(format!("efference-permissions-{n:016x}"));
    std::fs::create_dir_all(&dir).expect("scratch dir");
    ScratchDir(dir)
}

fn mode(path: &Path) -> u32 {
    std::fs::metadata(path)
        .expect("the path exists")
        .permissions()
        .mode()
        & 0o777
}

fn seed(path: &Path, mode: u32) {
    std::fs::write(path, b"x").expect("write");
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).expect("chmod");
}

/// Every state file a loose umask produced — current and legacy names alike —
/// lands owner-only, and the directory with it.
#[test]
fn loose_modes_are_tightened() {
    let dir = scratch();
    seed(&dir.join("state.db"), 0o644);
    seed(&dir.join("config.toml"), 0o664);
    seed(&dir.join("push.json"), 0o644);
    seed(&dir.join("totp-burns"), 0o644);
    seed(&dir.join("state.db.bak-seed"), 0o644);
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).expect("chmod dir");

    tighten_state_dir(&dir);

    assert_eq!(mode(&dir), 0o700);
    for name in STATE_FILES {
        assert_eq!(mode(&dir.join(name)), 0o600, "{name}");
    }
}

/// A mode stricter than the target is the operator's, not the daemon's to
/// widen; a partially loose one keeps its tighter bits and loses the rest.
#[test]
fn stricter_modes_are_kept() {
    let dir = scratch();
    seed(&dir.join("state.db"), 0o400);
    seed(&dir.join("config.toml"), 0o640);
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).expect("chmod dir");

    tighten_state_dir(&dir);

    assert_eq!(mode(&dir.join("state.db")), 0o400);
    assert_eq!(mode(&dir.join("config.toml")), 0o600);
    assert_eq!(mode(&dir), 0o700);
}

/// An absent state file is skipped without failing, and nothing is created:
/// the tightening runs on every boot, most of which see only some of the names.
#[test]
fn absent_files_are_skipped() {
    let dir = scratch();

    tighten_state_dir(&dir);

    assert_eq!(mode(&dir), 0o700);
    for name in STATE_FILES {
        assert!(!dir.join(name).exists(), "{name}");
    }
}

/// The socket is tightened after the bind mints it, whatever the umask left,
/// and the owner can still connect.
#[test]
fn the_socket_is_tightened_after_bind() {
    // A short absolute path: Darwin refuses to bind a socket past its 104-byte
    // `SUN_LEN`, and a session temp dir alone can spend most of that budget —
    // so the fixture binds under `/tmp` directly, not inside a scratch dir.
    let n = {
        use std::hash::{BuildHasher, Hasher, RandomState};
        RandomState::new().build_hasher().finish()
    };
    let socket = std::path::PathBuf::from(format!("/tmp/eprm-{n:08x}.sock"));
    let _listener = std::os::unix::net::UnixListener::bind(&socket).expect("bind");
    std::fs::set_permissions(&socket, std::fs::Permissions::from_mode(0o755)).expect("chmod");

    tighten_socket(&socket);

    assert_eq!(mode(&socket), 0o700);
    assert!(
        std::os::unix::net::UnixStream::connect(&socket).is_ok(),
        "the owner still connects"
    );
    std::fs::remove_file(&socket).expect("cleanup");
}
