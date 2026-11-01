//! Explicit session roots (docs/control-channel.md).

use std::io;
use std::path::{Path, PathBuf};

use crate::config::policy::Directories;

/// Boot paths supplied by the caller, independent of the host process cwd.
/// Storage does not follow the socket: a host may place its listener elsewhere.
#[derive(Clone, Debug)]
pub struct SessionPaths {
    /// The project (config, grants, session base) and the working directory (tools,
    /// children); the same directory unless the session works in a worktree.
    directories: Directories,
    storage: PathBuf,
}

impl SessionPaths {
    /// Require absolute paths rather than resolving against ambient process state.
    /// The caller creates storage and resolves its launch directory before boot.
    pub fn new(directories: Directories, storage: PathBuf) -> io::Result<Self> {
        for (name, path) in [
            ("project", directories.project()),
            ("working", directories.working()),
            ("storage", storage.as_path()),
        ] {
            if !path.is_absolute() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("session {name} must be absolute: {}", path.display()),
                ));
            }
        }
        Ok(Self {
            directories,
            storage,
        })
    }

    /// The project directory: where `efference.toml` and the session base live.
    pub fn project(&self) -> &Path {
        self.directories.project()
    }

    /// The working directory: where the session's tools and children run.
    pub fn working(&self) -> &Path {
        self.directories.working()
    }

    /// The directories the permission classifier resolves against (docs/permissions.md).
    pub fn directories(&self) -> Directories {
        self.directories.clone()
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
        self.storage.join(crate::session::USAGE_NAME)
    }

    pub(crate) fn attachments(&self) -> PathBuf {
        crate::session::attachments::dir_for_session(&self.storage)
    }

    /// The attachments directory as the model addresses it: relative to the working
    /// directory when inside it, absolute otherwise (a worktree session's attachments
    /// sit under the project, outside its working directory).
    pub(crate) fn attachment_display(&self) -> PathBuf {
        let dir = self.attachments();
        dir.strip_prefix(self.working())
            .unwrap_or(&dir)
            .to_path_buf()
    }
}

#[cfg(test)]
mod test;
