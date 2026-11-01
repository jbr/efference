//! The harness guide overlay — usage documents compiled into the binary and served
//! through the `read` tool at virtual paths (docs/context.md).
//!
//! The documents are `[context]` entries that are **born resolved**: each is a
//! `(name, description, body)` triple in [`DOCUMENTS`], entering the preamble as a
//! [`crate::record::log::context::Item::Listed`] stub without a frontmatter round-trip. Only the
//! *source* and the path *resolution* differ from an operator's group; the render is the
//! same code.
//!
//! A `read` of one of those paths is intercepted at dispatch ([`read_documents`],
//! `crate::agent::step`) and answered from the binary as an ordinary [`RawResult`], so
//! `project` never learns the result was special and no log or wire type changes. The
//! prefix is read-granted by the fast path (`crate::config::policy`) and never write-granted: the
//! harness cannot write into its own binary, and granting itself write on its own
//! documents would be meaningless.
//!
//! **Known edge, deliberately not half-implemented:** only `read` resolves on the prefix.
//! `list`, `search`, and the edit tools answer with [`reject_non_read`] instead — the stub
//! listing in the preamble is what substitutes for a directory listing.

use crate::record::log::{RawResult, ToolCall};

/// The reserved path prefix. Scheme-shaped rather than path-shaped so `read` can grow
/// other schemes later without re-deciding what a leading `/` means; the dispatch layer
/// only ever checks the front of a path against this string.
pub const SCHEME: &str = "efference://";

/// A mount under [`SCHEME`]. `[context]` names it as `{ builtin = "guide" }`;
/// [`BUILTIN`] is that config value and this is the path segment it serves at.
pub const MOUNT: &str = "efference://guide/";

/// The `[context]` value that lists this mount: `Label = { builtin = "guide" }`.
/// `guide/<slug>` names one document, which rides in full rather than as a stub
/// ([`builtin_section`]).
pub const BUILTIN: &str = "guide";

/// Whether a `builtin = "…"` value names something this binary ships: the mount,
/// or one of its documents by slug.
pub fn is_builtin(value: &str) -> bool {
    value == BUILTIN || inline_document(value).is_some()
}

/// The values [`is_builtin`] accepts, as the published project schema's `enum` for a
/// `[context]` entry's `builtin` (docs/config-schemas.md).
pub fn builtin_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
    let slugs: Vec<String> = DOCUMENTS
        .iter()
        .map(|d| format!("{BUILTIN}/{}", d.slug))
        .collect();
    crate::config::config_schemas::documented_enum(
        std::iter::once((BUILTIN, "Every document, each listed as a one-line stub.")).chain(
            slugs
                .iter()
                .map(String::as_str)
                .zip(DOCUMENTS.iter().map(|d| d.description)),
        ),
    )
}

/// The document a `guide/<slug>` builtin names.
fn inline_document(value: &str) -> Option<&'static Document> {
    let slug = value.strip_prefix(BUILTIN)?.strip_prefix('/')?;
    DOCUMENTS.iter().find(|d| d.slug == slug)
}

/// The group heading used when the harness lists the guide itself — the config-less boot,
/// where no operator wrote a label. An operator who lists the mount supplies their own.
pub const DEFAULT_HEADING: &str = "EfferenceGuide";

/// The `<description>` rendered above the stub listing, telling the model what the paths
/// are and that `read` is how they open.
pub const GROUP_DESCRIPTION: &str = "Documentation for this harness, compiled into the binary rather than stored on disk. \
     Read one of these paths with the `read` tool to load the document.";

/// One compiled-in document. `slug` is its address under [`MOUNT`] and its tag name in the
/// preamble; `description` is the one line that rides in the stub; `body` is what a `read`
/// returns.
pub struct Document {
    pub slug: &'static str,
    pub description: &'static str,
    pub body: &'static str,
    /// The tool names this document tells the reader to call. Not rendered — it is what
    /// the reference-validity test checks against the mounted table, so a renamed tool
    /// breaks the build rather than the reader (`test.rs`).
    pub tools: &'static [&'static str],
}

/// The document set. Order is the listing order in the preamble.
pub const DOCUMENTS: &[Document] = &[
    Document {
        slug: "new-project",
        description: "Setting up a project: where the config lives, what belongs in it, \
                      and the difference between the project template and a session's own copy.",
        body: include_str!("../../assets/guide/new-project.md"),
        tools: &["read"],
    },
    Document {
        slug: "advanced-config",
        description: "What a setup can offer when the project calls for it: mounting efference \
                      in another agent harness, secrets through [env], denying secret-bearing \
                      files, Claude Code memories, watch-for.",
        body: include_str!("../../assets/guide/advanced-config.md"),
        tools: &["read"],
    },
    Document {
        slug: "user-config",
        description: "Every key in the user-wide ~/.efference/config.toml: connections and \
                      their credentials, the three roles, env_hook (the environment sessions \
                      start with), [secrets], [host], [updates], [tui].",
        body: include_str!("../../assets/guide/user-config.md"),
        tools: &["read"],
    },
    Document {
        slug: "host",
        description: "The host that holds sessions, the web interface, reaching it over \
                      Tailscale, signing browsers in, and keeping the host running as a service.",
        body: include_str!("../../assets/guide/host.md"),
        tools: &["read"],
    },
    Document {
        slug: "config-format",
        description: "Reference for every `efference.toml` key: scope, tools, [commands], \
                      [paths], [context], [env], [bash], [transport], [mcp.servers].",
        body: include_str!("../../assets/guide/config-format.md"),
        tools: &["read"],
    },
    Document {
        slug: "editing",
        description: "Working with files: batched reads, the no-clobber write, and the \
                      anchor-based edit pipeline (edit / find_anchor / retarget_edit / undo_edit).",
        body: include_str!("../../assets/guide/editing.md"),
        tools: &[
            "read",
            "search",
            "list",
            "write",
            "edit",
            "find_anchor",
            "retarget_edit",
            "undo_edit",
            "bash",
        ],
    },
    Document {
        slug: "supervising",
        description: "Read before supervising agents you create with the agents__* tools: \
                      the verbs, what each status asks of you, gates and decide, questions, \
                      reading a transcript, and what an agent sees of your decisions.",
        body: include_str!("../../assets/guide/supervising.md"),
        tools: &[
            "agents__create",
            "agents__status",
            "agents__transcript",
            "agents__message",
            "agents__decide",
            "agents__interrupt",
            "agents__archive",
            "agents__hand_off",
            "wait",
            "ask",
            "read",
        ],
    },
    Document {
        slug: "artifacts",
        description: "Read before your first html-artifact fence: showing a chart, diagram or \
                      small page in the web transcript, its sandbox and sizing, and the \
                      stylesheet and variables every artifact gets.",
        body: include_str!("../../assets/guide/artifacts.md"),
        tools: &[],
    },
];

/// The virtual path a slug is addressed at.
pub fn path(slug: &str) -> String {
    format!("{MOUNT}{slug}")
}

/// Whether a `read` (or any other) target names the virtual mount rather than the
/// filesystem. The whole reserved space, not just the documents that exist — a typo'd slug
/// must be answered by the overlay ("no such document, here are the ones there are"), not
/// handed to a filesystem tool that would report a missing file.
pub fn is_virtual(target: &str) -> bool {
    target.starts_with(SCHEME)
}

/// The document at a virtual path, if the slug names one.
pub fn lookup(target: &str) -> Option<&'static Document> {
    let slug = target.strip_prefix(MOUNT)?;
    DOCUMENTS.iter().find(|d| d.slug == slug)
}

/// The `[context]` section this mount renders as, under `heading`. Built directly as
/// resolved items — the render half of the `[context]` pipeline, not the read-from-disk
/// half (docs/context.md).
pub fn section(heading: &str) -> crate::record::log::context::Section {
    crate::record::log::context::Section {
        heading: Some(heading.to_string()),
        description: Some(GROUP_DESCRIPTION.to_string()),
        items: DOCUMENTS
            .iter()
            .map(|d| crate::record::log::context::Item::Listed {
                name: d.slug.to_string(),
                description: d.description.to_string(),
                path: path(d.slug),
            })
            .collect(),
    }
}

/// The `[context]` section a builtin value renders as under the operator's
/// `heading`: the stub listing for the mount, or one document's full body for
/// `guide/<slug>`, tagged with the heading the way an `exec` section is.
pub fn builtin_section(
    value: &str,
    heading: &str,
    description: Option<String>,
) -> crate::record::log::context::Section {
    match inline_document(value) {
        Some(document) => crate::record::log::context::Section {
            heading: None,
            description,
            items: vec![crate::record::log::context::Item::Inline {
                name: heading.to_string(),
                path: path(document.slug),
                body: document.body.trim_end().to_string(),
            }],
        },
        None => {
            let mut section = section(heading);
            if description.is_some() {
                section.description = description;
            }
            section
        }
    }
}

/// Every path the mount serves, one per line — what a wrong slug and a wrong tool are both
/// told, so a reader that guessed can correct itself in one turn.
fn listing() -> String {
    DOCUMENTS
        .iter()
        .map(|d| format!("  {} — {}", path(d.slug), d.description))
        .collect::<Vec<_>>()
        .join("\n")
}

/// The path target(s) a call names, for the two hooks below. Reuses the classifier's own
/// per-tool key map so a tool's path field is named in exactly one place
/// (docs/permissions.md).
fn targets(call: &ToolCall) -> Vec<&str> {
    let Some((key, _, default)) = crate::config::policy::path_spec(&call.name) else {
        return Vec::new();
    };
    match call.input.get(key) {
        Some(serde_json::Value::String(s)) => vec![s.as_str()],
        Some(serde_json::Value::Array(items)) => {
            items.iter().filter_map(serde_json::Value::as_str).collect()
        }
        _ => default.into_iter().collect(),
    }
}

/// A call that names a virtual path but is not a `read` — answered before the permission
/// cascade, because nothing any rung could say would make it run and a gate spent on it
/// teaches the operator nothing.
pub fn reject_non_read(call: &ToolCall) -> Option<RawResult> {
    if call.name == "read" || !targets(call).iter().any(|t| is_virtual(t)) {
        return None;
    }
    Some(RawResult::error(format!(
        "`{}` does not resolve on `{SCHEME}` paths. These are documents compiled into \
         the harness binary, not files: `read` is the only tool that opens them, and \
         nothing writes to them.\n\nThe documents are:\n{}",
        call.name,
        listing()
    )))
}

/// Serve a `read` of the virtual mount from the binary, or `None` when the call names no
/// virtual path and the real `read` should run.
///
/// A batch mixing virtual and real paths is refused rather than split: the two halves come
/// from different places, and answering half a batch silently would be worse than one
/// corrective turn.
pub fn read_documents(call: &ToolCall) -> Option<RawResult> {
    if call.name != "read" {
        return None;
    }
    let targets = targets(call);
    if !targets.iter().any(|t| is_virtual(t)) {
        return None;
    }
    if let Some(real) = targets.iter().find(|t| !is_virtual(t)) {
        return Some(RawResult::error(format!(
            "this `read` mixes harness documents with a file on disk (`{real}`). They are \
             served from different places, so read them in separate calls.",
        )));
    }
    let mut bodies = Vec::with_capacity(targets.len());
    for target in &targets {
        match lookup(target) {
            Some(document) => bodies.push(document.body.trim_end().to_string()),
            None => {
                return Some(RawResult::error(format!(
                    "no harness document at `{target}`. The documents are:\n{}",
                    listing()
                )));
            }
        }
    }
    Some(RawResult::text(bodies.join("\n\n---\n\n")))
}

#[cfg(test)]
mod test;
