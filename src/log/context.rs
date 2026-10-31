//! The resolved `[context]` a boot preamble records (docs/context.md,
//! docs/log-and-projection.md).
//!
//! These are the shapes the log stores. Reading the configured files into them, and
//! rendering them as the preamble's `<name>…</name>` tags, is [`crate::context`].

use serde::{Deserialize, Serialize};

/// A fully-resolved `[context]` ready to render into the preamble: sections in **document order**
/// (docs/context.md). Each [`Section`] is a bare entry (no heading) or a labeled group.
/// This is what [`crate::env`] builds from disk at boot and freezes into the cached prefix.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[cfg_attr(
    test,
    derive(schemars::JsonSchema),
    schemars(rename = "ResolvedContext")
)]
pub struct Resolved {
    pub sections: Vec<Section>,
}

/// One rendered section: a labeled group (`heading = Some`) or a bare entry (`heading = None`).
/// `description` is the operator's prose hint about how to use these files (e.g. how to read or
/// write a memory), rendered as a `<description>` child element inside the group wrapper.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(
    test,
    derive(schemars::JsonSchema),
    schemars(rename = "ContextSection")
)]
pub struct Section {
    pub heading: Option<String>,
    pub items: Vec<Item>,
    pub description: Option<String>,
}

/// A single resolved context item — an inlined file (body in full) or a listed stub (read on demand).
/// Both render as a name tag `<name>…</name>` (docs/context.md); the name is the file's
/// frontmatter `name`, else its basename.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema), schemars(rename = "ContextItem"))]
pub enum Item {
    /// The full body rides in the preamble inside `<name>…</name>`. `path` is kept for provenance /
    /// the boot snapshot, not rendered (the tag name is the identity).
    Inline {
        name: String,
        path: String,
        body: String,
    },
    /// A one-line stub `<name>{description} Read {path} for the full file.</name>`; the body is read
    /// on demand via `path`.
    Listed {
        name: String,
        description: String,
        path: String,
    },
    /// A dynamic section (docs/context.md): `body` is the stdout of `command`, run
    /// **once at boot** with the session cwd — frozen and snapshotted exactly like a static file, so
    /// `Date = { exec = "date" }` honestly means "the date as of session boot". Renders identically
    /// to [`Item::Inline`]; `command` is kept for provenance, not rendered.
    Exec {
        name: String,
        command: String,
        body: String,
    },
}
