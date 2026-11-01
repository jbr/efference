//! What this module refuses (docs/log-and-projection.md).
//!
//! One type per way in: [`LoadError`] for reading a log, [`OpenError`] for
//! claiming one to append to. Both name the file they are about and both keep
//! the *class* of the failure, so a caller can tell an unreadable recording
//! from a log another writer holds without reading a message — an inventory
//! lists the first and leaves the second alone (docs/README.md).
//!
//! `Display` is the text these errors have always printed — `"{path}:{line}:
//! …"`, which is what a person wants at a terminal — and the variants are what
//! a program wants. [`LoadError`] converts into an [`io::Error`] through
//! [`From`], so a caller whose signature is already `io::Result` keeps its `?`
//! and its error kind.

use std::fmt::Display;
use std::io;
use std::path::{Path, PathBuf};

use super::LogHeader;

/// Why a log could not be read. The classes are the distinctions a caller acts
/// on: an inventory lists a recording it cannot read *by the reason*, and a
/// writable reopen refuses every one of them without writing a byte, since a
/// file this build cannot read is a file it cannot extend.
#[derive(Debug)]
pub enum LoadError {
    /// The file could not be opened or read — it is not there, or the
    /// filesystem said no.
    Io { path: PathBuf, error: io::Error },
    /// Line 1 carries no `format`, so the file is either a log written before
    /// the header existed or something that is not a session log at all. There
    /// is no importer for the first case.
    Unversioned { path: PathBuf },
    /// Line 1 names a different file type: a usage report, another tool's
    /// journal, anything but this one.
    WrongFormat { path: PathBuf, found: String },
    /// The format is this one and the version is not a version this build
    /// reads. Settled before the metadata is read, so a newer file's metadata
    /// cannot report as the reason to stop.
    UnsupportedVersion { path: PathBuf, version: u64 },
    /// Line `line` is not what a log of this version puts there: a header that
    /// is not a JSON object, a blank line, a line that is not an entry. The
    /// damage is in the record rather than at its end.
    Corrupt {
        path: PathBuf,
        line: usize,
        message: String,
    },
    /// The entry on line `line` carries `found` where its position in the file
    /// says it must be `expected` — a duplicated or skipped `seq`, which no
    /// reader can take as one continuous sequence.
    Sequence {
        path: PathBuf,
        line: usize,
        expected: u64,
        found: u64,
    },
}

impl LoadError {
    /// The file this is about, whichever way the load failed.
    pub fn path(&self) -> &Path {
        match self {
            LoadError::Io { path, .. }
            | LoadError::Unversioned { path }
            | LoadError::WrongFormat { path, .. }
            | LoadError::UnsupportedVersion { path, .. }
            | LoadError::Corrupt { path, .. }
            | LoadError::Sequence { path, .. } => path,
        }
    }

    pub(super) fn io(path: &Path, error: io::Error) -> Self {
        LoadError::Io {
            path: path.to_path_buf(),
            error,
        }
    }

    pub(super) fn corrupt(path: &Path, line: usize, message: impl Display) -> Self {
        LoadError::Corrupt {
            path: path.to_path_buf(),
            line,
            message: message.to_string(),
        }
    }
}

impl Display for LoadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LoadError::Io { path, error } => write!(f, "{}: {error}", path.display()),
            LoadError::Unversioned { path } => write!(
                f,
                "{}:1: line 1 carries no `format`: this is not a log this build reads — either a \
                 log written before the header existed, or a different file",
                path.display()
            ),
            LoadError::WrongFormat { path, found } => write!(
                f,
                "{}:1: unknown log format {found:?}; this build reads {:?}",
                path.display(),
                LogHeader::FORMAT
            ),
            LoadError::UnsupportedVersion { path, version } => write!(
                f,
                "{}:1: unsupported log version {version}; this build reads version {}",
                path.display(),
                LogHeader::VERSION
            ),
            LoadError::Corrupt {
                path,
                line,
                message,
            } => write!(f, "{}:{line}: {message}", path.display()),
            LoadError::Sequence {
                path,
                line,
                expected,
                found,
            } => write!(
                f,
                "{}:{line}: entry carries seq {found} but sits at index {expected}: seq must \
                 equal the entry's index, so a duplicated or skipped seq is a corrupt log",
                path.display()
            ),
        }
    }
}

impl std::error::Error for LoadError {}

impl From<LoadError> for io::Error {
    fn from(load: LoadError) -> Self {
        match &load {
            // An `io::Error` keeps its kind, so a caller matching on
            // `NotFound` or `PermissionDenied` still sees it through here.
            LoadError::Io { error, .. } => io::Error::new(error.kind(), load.to_string()),
            _ => io::Error::new(io::ErrorKind::InvalidData, load.to_string()),
        }
    }
}

/// Why a log could not be reopened for append. The three are the three steps
/// that can refuse, in the order they are tried (docs/log-and-projection.md).
#[derive(Debug)]
pub enum OpenError {
    /// Another writer holds this log. Its lock is out and its process is
    /// still appending, so this path is a session in use rather than one
    /// waiting to be continued.
    Held { path: PathBuf },
    /// The file is not a log this build reads, or is damaged past the point
    /// where appending would mean guessing. Nothing was written.
    Load(LoadError),
    /// The file could not be opened, or the tail could not be repaired.
    Io { path: PathBuf, error: io::Error },
}

impl OpenError {
    pub(super) fn io(path: &Path, error: io::Error) -> Self {
        OpenError::Io {
            path: path.to_path_buf(),
            error,
        }
    }
}

impl Display for OpenError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            OpenError::Held { path } => write!(
                f,
                "{}: another writer holds this log open; it can be reopened once that writer is gone",
                path.display()
            ),
            OpenError::Load(error) => Display::fmt(error, f),
            OpenError::Io { path, error } => write!(f, "{}: {error}", path.display()),
        }
    }
}

impl std::error::Error for OpenError {}

impl From<OpenError> for io::Error {
    fn from(open: OpenError) -> Self {
        match &open {
            OpenError::Io { error, .. } => io::Error::new(error.kind(), open.to_string()),
            OpenError::Held { .. } => io::Error::new(io::ErrorKind::WouldBlock, open.to_string()),
            OpenError::Load(_) => io::Error::new(io::ErrorKind::InvalidData, open.to_string()),
        }
    }
}
