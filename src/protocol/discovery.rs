//! Composer discovery (docs/control-api.md): the entries one directory offers for a
//! typed path prefix, and the host's sessions for a typed `@` prefix. Both answers
//! come from names and metadata only, and neither is logged.

use crate::protocol::sessions::ListedSession;
use crate::record::log::ReferenceTargetKind;
use serde::{Deserialize, Serialize};

/// The answer to `GET /discovery/paths`: what one directory holds under the
/// composer's prefix (docs/control-api.md).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
pub struct PathCandidates {
    /// The prefix as sent, echoed so a client can drop an answer that arrived after
    /// the composer moved on.
    pub prefix: String,
    /// The directory the prefix resolved to, absolute and lexically normalized: the
    /// parent every `entries[].path` sits under. It is not necessarily the spelling
    /// the composer typed. Empty when that path has no UTF-8 spelling; `problem` is
    /// then set, and no entry is offered under a rewritten path.
    pub directory: String,
    /// The fragment after the last `/`, matched against each entry name.
    pub filter: String,
    /// More candidates exist than the limit returned. The limit bounds the answer and
    /// the stats that build it, not the directory read: every entry name is examined
    /// and the matching names are sorted in full, then candidates are statted in name
    /// order only until one past the limit exists.
    pub truncated: bool,
    /// Why there are no entries — a missing or unreadable directory, a `~` prefix
    /// with no home directory, or a directory whose own path has no UTF-8 spelling.
    /// Absent when the directory was read.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub problem: Option<String>,
    pub entries: Vec<PathCandidate>,
}

/// One entry a composer can offer. `path` is what a submission stores; `original` is
/// the spelling to keep beside it, so the reference reads the way the composer wrote
/// it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
pub struct PathCandidate {
    /// The entry's own name: one path component, no separator.
    pub name: String,
    /// The directory spelling as typed with `name` joined onto it — the value a
    /// `SubmittedTarget::Filesystem` keeps as its `original`.
    pub original: String,
    /// Absolute and lexically normalized, with no symbolic link resolved and no
    /// worktree path mapped to its project counterpart. An entry is offered only when
    /// its path round-trips; one that would need a rewritten spelling is omitted.
    pub path: String,
    /// What the entry is now, following a symbolic link for the kind.
    pub target: ReferenceTargetKind,
    /// The entry itself is a symbolic link; `target` follows it.
    pub symlink: bool,
}

/// The answer to `GET /discovery/agents`: the host's sessions whose ids start with
/// the composer's prefix (docs/control-api.md).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
pub struct AgentCandidates {
    /// The prefix as sent, `@` included, echoed like [`PathCandidates::prefix`].
    pub prefix: String,
    /// The prefix matched more sessions than the limit.
    pub truncated: bool,
    /// Why there are no candidates — a listener that serves one session has no host
    /// registry to list. Absent on a host.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub problem: Option<String>,
    /// The rows `GET /sessions` returns, in id order.
    pub agents: Vec<ListedSession>,
}

#[cfg(test)]
mod test;
