//! A store directory addressed by name — the composer's upload surface (docs/inbox.md).
//!
//! A composer attaches a file by uploading it (`POST /attachments`), and the turn that carries it
//! names the upload. Everything a client can say about a stored file goes through a bare name, so
//! this is where "a name is one path component inside this directory" is enforced: a name that
//! [`sanitize`](super::sanitize) would change is not one this store handed out, and is refused
//! rather than resolved.

use super::{Arrival, inspect, sanitize, store};
use std::io;
use std::path::{Path, PathBuf};

/// A store directory, and how the model addresses it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Store {
    /// Where the bytes are.
    dir: PathBuf,
    /// The same directory as the notice names it — relative to the session's working root when
    /// the store is inside it (docs/control-channel.md).
    display: PathBuf,
}

impl Store {
    pub fn new(dir: impl Into<PathBuf>, display: impl Into<PathBuf>) -> Self {
        Self {
            dir: dir.into(),
            display: display.into(),
        }
    }

    /// The composer's subdirectory of this store ([`COMPOSER_DIR`](super::COMPOSER_DIR)).
    pub fn composer(&self) -> Self {
        Self::new(
            self.dir.join(super::COMPOSER_DIR),
            self.display.join(super::COMPOSER_DIR),
        )
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Write `bytes` under a name derived from `suggested` — suffixed, never overwriting — and
    /// describe what landed.
    pub fn put(&self, suggested: &str, bytes: &[u8]) -> io::Result<Arrival> {
        let full = store(&self.dir, suggested, bytes)?;
        self.describe(&full)
    }

    /// Describe the stored file called `name`. `NotFound` for a name that is not a single
    /// component or names nothing.
    pub fn arrival(&self, name: &str) -> io::Result<Arrival> {
        self.describe(&self.path(name)?)
    }

    /// Where the file called `name` is, when it exists.
    pub fn path(&self, name: &str) -> io::Result<PathBuf> {
        if name.is_empty() || sanitize(name) != name {
            return Err(io::ErrorKind::NotFound.into());
        }
        let full = self.dir.join(name);
        if std::fs::metadata(&full)?.is_file() {
            Ok(full)
        } else {
            Err(io::ErrorKind::NotFound.into())
        }
    }

    /// Remove the file called `name` — a staged attachment the operator took back before sending.
    pub fn remove(&self, name: &str) -> io::Result<()> {
        std::fs::remove_file(self.path(name)?)
    }

    fn describe(&self, full: &Path) -> io::Result<Arrival> {
        let name = full.file_name().unwrap_or_default();
        inspect(&self.display.join(name).to_string_lossy(), full)
    }
}
