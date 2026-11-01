//! The row the archived-session scan produces for one session directory (docs/control-api.md):
//! what `GET /sessions?state=archived` lists, flattened into
//! [`ArchivedSession`](super::sessions::ArchivedSession). The scan that fills it is
//! [`crate::host::inventory`].

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// One session directory under a root's `.efference/`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
pub struct InventoryRow {
    /// The directory name.
    pub id: String,
    /// The working root whose `.efference` holds the directory.
    pub cwd: PathBuf,
    /// The text of the last `Title` entry, when the log has one
    /// (docs/session-title.md).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// `at` of the first entry after the header, epoch milliseconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_at: Option<u64>,
    /// `at` of the last complete entry, epoch milliseconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_active_at: Option<u64>,
    /// Set when the last lifecycle entry (`Resumed` or `Archived`) is not `Archived`:
    /// the session was open when its host went away, rather than archived on purpose.
    /// A log with neither entry is a session that was never resumed or archived, so
    /// it counts as open too (docs/resumption.md).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub interrupted: bool,
    /// Who archived the session, from the `source` of the `Archived` entry that is the
    /// last lifecycle entry; absent when it records none or the last lifecycle entry is
    /// not `Archived` (docs/log-and-projection.md).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub archived_by: Option<String>,
    /// Why this directory is not a readable session, when it is not. A row with
    /// a problem keeps whatever the scan did learn.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub problem: Option<String>,
}

impl InventoryRow {
    pub(crate) fn new(id: &str, cwd: &Path) -> Self {
        Self {
            id: id.to_string(),
            cwd: cwd.to_path_buf(),
            title: None,
            created_at: None,
            last_active_at: None,
            interrupted: false,
            archived_by: None,
            problem: None,
        }
    }

    pub(crate) fn with_problem(mut self, problem: impl Into<String>) -> Self {
        self.problem = Some(problem.into());
        self
    }
}
