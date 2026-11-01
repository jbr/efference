//! The log and its deterministic views: the names a recorded entry carries, the log
//! and its file, what a log's last completion left undone, the wire projection, the
//! fenced-artifact syntax, and the session directory's layout
//! (docs/README.md, *Dependency direction*).

pub(crate) mod artifact;
pub mod log;
pub mod names;
pub mod persist;
pub mod project;
pub mod recovery;
pub mod session_dir;
