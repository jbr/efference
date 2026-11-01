//! A directory listing from the `/files` surface (docs/web-bind.md): the entries of one
//! directory, flat or walked below it.
//!
//! The walk, the caps and the ignore set that decide what a listing holds are the host's
//! (`daemon/files/list.rs`). A listing is serialized straight from these types, so the
//! fields reach the bytes in the order declared here (`daemon/files/test/listing.rs`).

use serde::{Deserialize, Serialize};

use crate::record::log::Timestamp;

/// A directory's listing. `path` is the directory's own path relative to the root, empty at
/// the root itself, so a client can render a breadcrumb without reconstructing it from the
/// URL.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
pub struct DirListing {
    pub path: String,
    /// Whether entry names are paths (recursive) or bare names (flat).
    pub recursive: bool,
    /// A cap was hit: the set is a prefix of what is there, not the whole of it.
    pub truncated: bool,
    pub entries: Vec<DirEntry>,
}

/// One entry of a directory listing. `kind` follows symlinks (a link to a directory lists as
/// a directory) while `symlink` records that the entry itself is one, so a client can show
/// the indirection without a second request.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
pub struct DirEntry {
    /// Bare in a flat listing; relative to the listed directory in a recursive one.
    pub name: String,
    pub kind: DirEntryKind,
    /// Bytes on disk. Reported for directories too, where it is the directory's own size and
    /// means nothing to a reader.
    pub size: u64,
    /// Absent (`null`) for a link whose target does not resolve.
    pub modified_at: Option<Timestamp>,
    pub symlink: bool,
}

/// What a listed entry is, after following a symlink.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum DirEntryKind {
    File,
    Dir,
    /// A device, socket, fifo — or a symlink whose target does not resolve.
    Other,
}
