//! Why an assistant entry is record-only (docs/log-and-projection.md, docs/transport.md).

use serde::{Deserialize, Serialize};

use super::UpstreamFailureRecord;

/// Why an assistant entry is **record-only**: it stays in the log for audit, projection
/// skips it, and the loop asks again (docs/transport.md). Skipping it is what lets the
/// next request re-send the same bytes and ride the cached prefix.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
pub enum Exclusion {
    /// The upstream produced no usable turn: the stream died before finalizing, finalized
    /// with no content and no usage, or the request itself failed. The provider has no
    /// record of the generation and does not stand behind it.
    Upstream(UpstreamFailureRecord),
    /// The model finalized a completion with neither prose nor a call: reasoning alone, or
    /// text that is empty or whitespace. The provider stands behind it, but it ends
    /// nothing a reader can act on, so it is not taken as the turn's end.
    Empty,
}

impl Exclusion {
    /// What a reader is told about why the entry is record-only: the upstream's own words,
    /// or what the model did.
    pub fn why(&self) -> &str {
        match self {
            Self::Upstream(record) => record.why(),
            Self::Empty => "the model finalized with neither prose nor a call",
        }
    }

    /// The upstream's failure, when that is why the entry is record-only.
    pub fn upstream(&self) -> Option<&UpstreamFailureRecord> {
        match self {
            Self::Upstream(record) => Some(record),
            Self::Empty => None,
        }
    }
}
