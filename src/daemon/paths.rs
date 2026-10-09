//! Explicit session roots (docs/control-channel.md).

use std::io;
use std::path::{Path, PathBuf};

/// Boot paths supplied by the caller, independent of the host process cwd.
/// Storage does not follow the socket: a host may place its listener elsewhere.
#[derive(Clone, Debug)]
pub struct SessionPaths {
    cwd: PathBuf,
    storage: PathBuf,
}

impl SessionPaths {
    /// Require absolute paths rather than resolving against ambient process state.
    /// The caller creates storage and resolves its launch directory before boot.
    pub fn new(cwd: PathBuf, storage: PathBuf) -> io::Result<Self> {
        for (name, path) in [("cwd", &cwd), ("storage", &storage)] {
            if !path.is_absolute() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("session {name} must be absolute: {}", path.display()),
                ));
            }
        }
        Ok(Self { cwd, storage })
    }

    pub fn cwd(&self) -> &Path {
        &self.cwd
    }

    /// The directories the permission classifier resolves against (docs/permissions.md).
    pub fn directories(&self) -> crate::policy::Directories {
        crate::policy::Directories::at(&self.cwd)
    }

    pub fn storage(&self) -> &Path {
        &self.storage
    }

    /// The session id: the storage directory's name (docs/session-title.md).
    pub fn id(&self) -> &str {
        self.storage
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_default()
    }

    pub(crate) fn usage_log(&self) -> PathBuf {
        self.storage.join("usage.jsonl")
    }

    pub(crate) fn attachments(&self) -> PathBuf {
        crate::attachments::dir_for_session(&self.storage)
    }

    pub(crate) fn attachment_display(&self) -> PathBuf {
        let dir = self.attachments();
        dir.strip_prefix(&self.cwd).unwrap_or(&dir).to_path_buf()
    }
}

#[cfg(test)]
mod test;
