//! The remembered launch locations (docs/control-api.md): the entries `GET /projects`
//! lists, and the identity `DELETE /projects` forgets one by.
//!
//! The index of entries, their availability against the filesystem, and the store they
//! come from are the host's (`daemon::host`, `remembered`); an entry is built from a stored
//! row there.

use serde::{Deserialize, Serialize};

/// One remembered launch location. `cwd` and `config` are exactly what `POST /sessions`
/// takes back, so an entry replays a launch verbatim.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
pub struct ProjectEntry {
    /// The directory's own name, or its full path where it has none (a filesystem root).
    pub label: String,
    pub cwd: String,
    /// The configuration the launch named, as it was named. `null` asks creation to probe,
    /// which is what a launch that named none did.
    pub config: Option<String>,
    /// The identity the index stored the entry under, which `DELETE /projects` takes.
    /// Reported rather than recomputed, because recomputing re-probes the filesystem.
    pub config_key: String,
    /// Unix seconds.
    pub last_used_at: i64,
}

/// The entries of `GET /projects`, most recently used first.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
pub struct ProjectList {
    pub projects: Vec<ProjectEntry>,
}

/// What names one entry for `DELETE /projects`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
pub struct ProjectIdentity {
    pub cwd: String,
    pub config_key: String,
}

/// The answer to `DELETE /projects`: the identity that was forgotten.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
pub struct ForgottenProject {
    pub forgotten: ProjectIdentity,
}
