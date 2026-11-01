//! A session's `/files` view (docs/web-bind.md): the canonical directory the `/files`
//! routes serve, the watcher behind their change streams, and the discovery ignore set.
//! The host keeps one on each session's entry and a session bind builds its own; the
//! routes that read it are `files.rs`, mounted on a web transport only.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::platform::fswatch::FsWatch;
use ignores::Ignores;

pub(crate) mod ignores;

/// The viewer's read surface, rooted at one canonical directory. A root that could
/// not be canonicalized at boot serves nothing rather than falling back to a wider
/// one.
#[derive(Debug, Clone)]
pub struct Files {
    pub(crate) root: Option<Arc<PathBuf>>,
    /// The core watcher behind the change streams (`crate::platform::fswatch`). `None` if
    /// the watcher could not start, in which case streams are refused while
    /// reads keep working.
    pub(crate) watch: Option<Arc<FsWatch>>,
    /// The discovery ignore set (`ignores`): listings and directory streams
    /// consult it, the read route and single-file streams never do.
    pub(crate) ignores: Option<Arc<Ignores>>,
}

impl Files {
    /// Rooted at the supplied session directory.
    pub(crate) fn rooted_at(root: impl AsRef<Path>) -> Self {
        let root = root.as_ref().canonicalize().ok().map(Arc::new);
        Self {
            ignores: root.as_ref().map(|root| Arc::new(Ignores::at(&***root))),
            root,
            watch: FsWatch::new().ok(),
        }
    }
}
