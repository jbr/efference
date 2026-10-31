//! Tests for `/files/**` (docs/web-bind.md), over the same in-memory transport the
//! rest of the daemon's HTTP tests use.
//!
//! Two of these are the point rather than coverage: that a symlink pointing out of
//! the root is refused (the containment re-check after `canonicalize`), and that a
//! file's own type never reaches `content-type`.

use super::*;
use trillium_testing::TestServer;

/// A scratch directory, removed when the guard drops. Canonicalized on the way out
/// because macOS hands back `/var/…` for a `/private/var/…` temp dir, and the
/// handler compares canonical paths.
struct Scratch(PathBuf);

impl Scratch {
    fn new(name: &str) -> Self {
        use std::hash::{BuildHasher, Hasher, RandomState};
        let n = RandomState::new().build_hasher().finish();
        let dir = std::env::temp_dir().join(format!("efference-files-{name}-{n:016x}"));
        std::fs::create_dir_all(&dir).expect("scratch dir");
        Self(dir.canonicalize().expect("canonical scratch"))
    }

    fn path(&self) -> &Path {
        &self.0
    }

    fn write(&self, rel: &str, contents: impl AsRef<[u8]>) -> PathBuf {
        let path = self.0.join(rel);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("parent");
        }
        std::fs::write(&path, contents).expect("write");
        path
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A status nothing else here answers with, so "the file surface passed this
/// through" is distinguishable from "the file surface answered 404".
const FELL_THROUGH: u16 = 418;

/// The handler with a marker behind it, standing in for the web client's surface,
/// which is mounted after it on the real stack.
async fn server(scratch: &Scratch) -> TestServer<impl trillium::Handler> {
    TestServer::new((
        handler(Files::rooted_at(scratch.path())),
        |conn: Conn| async move { conn.with_status(FELL_THROUGH).halt() },
    ))
    .await
}

mod listing;
mod read;
mod streams;
