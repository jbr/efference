//! Line 1 of a persisted log: the format/version header, and the session
//! metadata boot records beside it (docs/log-and-projection.md).
//!
//! The header is one JSON object and nothing else. It is **not** a
//! [`LogEntry`](crate::log::LogEntry): it takes no `seq`, `project` never sees it,
//! and the entries behind it are numbered from 0 exactly as they would be without it.
//!
//! Readers require it. A file whose first line is an entry is an unversioned
//! log, not a v1 log missing a field, and the two are told apart by an error
//! rather than guessed at — so [`LogHeader::FORMAT`] and
//! [`LogHeader::VERSION`] are the only values the type can carry, and a file
//! claiming anything else is refused before a single byte is written.
//!
//! The metadata types here are the log's own. They deliberately do not reuse
//! the live configuration or registry types: those describe what a host will
//! do next, while a header records what a *past* run did, and the two drift
//! apart as either side grows fields. Boot copies values in once, the way it
//! already does for [`LoggedInference`].

use std::io::{self, Write};
use std::path::PathBuf;

use serde::ser::SerializeStruct;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::log::{LoggedInference, TriageModel};

/// What boot recorded about the session it created: the address it answers to,
/// the directory it ran in, the three inferences it was configured with, and
/// the knobs and lineage a reader needs to describe or continue the run.
///
/// A field with an obvious "off" value is skipped when `None`/empty, the same
/// way `LogEntry`'s optional fields are, so a header stays small and adding a
/// field later does not rewrite the files that predate it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionMetadata {
    pub id: String,
    /// The session's project directory: where `efference.toml` and the session base
    /// live (docs/control-channel.md).
    pub cwd: PathBuf,
    /// The working directory, recorded only when it differs from the project (a
    /// session working in a worktree), so a header without it reads as the two being
    /// the same directory.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub working: Option<PathBuf>,
    pub main: LoggedInference,
    pub spotter: LoggedInference,
    pub triage: TriageModel,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temperature: Option<u8>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub upstream: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_by: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub succeeds: Option<String>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub autonomous: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub keep_scratch: bool,
    /// Whether the session outlives its creator (docs/tools.md). A resume reads it
    /// from here; a header without it is a session that ends with its creator.
    #[serde(default, skip_serializing_if = "is_false")]
    pub detached: bool,
}

impl SessionMetadata {
    /// The five fields every session has an answer for by construction —
    /// address, working directory, and one inference per role. Everything else
    /// starts at its "not set" value, which a boot with the knob configured
    /// overwrites.
    pub fn new(
        id: impl Into<String>,
        cwd: impl Into<PathBuf>,
        main: LoggedInference,
        spotter: LoggedInference,
        triage: TriageModel,
    ) -> Self {
        Self {
            id: id.into(),
            cwd: cwd.into(),
            working: None,
            main,
            spotter,
            triage,
            temperature: None,
            effort: None,
            upstream: Vec::new(),
            created_by: None,
            succeeds: None,
            autonomous: false,
            keep_scratch: false,
            detached: false,
        }
    }
}

fn is_false(flag: &bool) -> bool {
    !*flag
}

/// The first line of a persisted log: the format and version that say how to
/// read the rest, plus the session's metadata.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LogHeader {
    metadata: Option<SessionMetadata>,
}

impl LogHeader {
    /// The `format` value every efference session log carries. A different
    /// string is a different file type, not a version to try.
    pub const FORMAT: &'static str = "efference-session-log";
    /// The log format this build reads. Bumped only when entries themselves
    /// change shape; metadata fields are additive and do not bump it.
    pub const VERSION: u32 = 7;

    /// A header carrying `metadata`, or none — a session persisted without it
    /// (a fixture, or a run that had no boot metadata to record).
    pub fn new(metadata: Option<SessionMetadata>) -> Self {
        Self { metadata }
    }

    /// What boot recorded, when this file carries it.
    pub fn metadata(&self) -> Option<&SessionMetadata> {
        self.metadata.as_ref()
    }

    /// Write the header as line 1, newline included. What `Log::create` calls
    /// before it returns, so a file that exists is already a readable log.
    pub fn write_to(&self, out: &mut impl Write) -> io::Result<()> {
        serde_json::to_writer(&mut *out, self)?;
        out.write_all(b"\n")
    }
}

impl Serialize for LogHeader {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut header = serializer.serialize_struct("LogHeader", 3)?;
        header.serialize_field("format", Self::FORMAT)?;
        header.serialize_field("version", &Self::VERSION)?;
        header.serialize_field("metadata", &self.metadata)?;
        header.end()
    }
}

/// Line 1 as it arrives from a file: every field optional, so each way it can
/// be wrong gets its own error instead of one generic "missing field". An
/// entry line parses as all-`None` here, which is how an unversioned log is
/// told apart from a header.
///
/// `metadata` is held as a raw value rather than deserialized, so the format
/// and version are settled before anything is read into the types of *this*
/// version. A file written by a newer build whose metadata has outgrown these
/// types is then refused as the unsupported version it is, instead of failing
/// on a field this build happens not to know.
#[derive(Deserialize)]
struct RawHeader {
    format: Option<String>,
    version: Option<u32>,
    metadata: Option<serde_json::Value>,
}

impl<'de> Deserialize<'de> for LogHeader {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = RawHeader::deserialize(deserializer)?;
        let Some(format) = raw.format else {
            return Err(serde::de::Error::custom(
                "no `format` field: this is not an efference session log — either an unversioned \
                 log written before the header, or a different file",
            ));
        };
        if format != Self::FORMAT {
            return Err(serde::de::Error::custom(format!(
                "unknown log format {format:?}; this build reads {:?}",
                Self::FORMAT
            )));
        }
        match raw.version {
            None => Err(serde::de::Error::custom(
                "header names a format but carries no `version`",
            )),
            Some(version) if version != Self::VERSION => Err(serde::de::Error::custom(format!(
                "unsupported log version {version}; this build reads version {}",
                Self::VERSION
            ))),
            Some(_) => {
                let metadata = raw
                    .metadata
                    .map(serde_json::from_value)
                    .transpose()
                    .map_err(|error| {
                        serde::de::Error::custom(format!("unreadable session metadata: {error}"))
                    })?;
                Ok(Self::new(metadata))
            }
        }
    }
}
