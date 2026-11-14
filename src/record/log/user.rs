//! A structured user document (docs/log-and-projection.md): the ordered
//! content sequence and sibling reference definitions a submitted turn
//! carries. One document is the sole [`super::ContentBlock::Document`] block
//! of a `User` entry, and the authoritative body of an `ask` reply that
//! referenced anything (`RawResult::document`).

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::attachment::Arrival;
use super::reference::{ReferenceDefinition, ReferenceTarget};
use crate::record::names::sources;

/// One ordered element of the document's content sequence: prose, or a
/// reference occurrence pointing at a definition id. The only two shapes a
/// user document carries — the thinking-block and tool shapes are
/// structurally absent.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
pub enum UserSegment {
    Text(String),
    /// One occurrence: the id of a definition in the same document's
    /// `references`.
    Reference(String),
}

/// A structured user document: prose and reference occurrences in one ordered
/// sequence, plus the definitions the occurrences point at.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
pub struct UserContent {
    /// Rendered by verbatim concatenation — no separator is inserted between
    /// segments, so text on either side of an occurrence survives exactly.
    pub content: Vec<UserSegment>,
    /// Definitions keyed by id. A `BTreeMap`, so JSON keys serialize sorted
    /// and the rendered block lists definitions in this order.
    pub references: BTreeMap<String, ReferenceDefinition>,
}

impl UserContent {
    /// A document of one prose segment.
    pub fn text(text: impl Into<String>) -> Self {
        Self {
            content: vec![UserSegment::Text(text.into())],
            references: BTreeMap::new(),
        }
    }

    /// Whether the document carries no references — a plain prose document.
    pub fn is_plain(&self) -> bool {
        self.references.is_empty()
    }

    /// Whether the document would deliver nothing: no non-blank prose and no
    /// occurrences. A blank submit keeps the loop idling rather than landing
    /// an empty `User` entry on the wire.
    pub fn is_blank(&self) -> bool {
        self.references.is_empty()
            && self.content.iter().all(|segment| match segment {
                UserSegment::Text(text) => text.trim().is_empty(),
                UserSegment::Reference(_) => false,
            })
    }

    /// The resolved uploads behind the document's attachment definitions, in
    /// id order — what a client stages again when it recalls the turn, and
    /// what a visible fallback lists beside [`readable`](Self::readable),
    /// which renders an attachment as nothing.
    pub fn attachment_arrivals(&self) -> Vec<Arrival> {
        self.references
            .values()
            .filter_map(|definition| match &definition.target {
                ReferenceTarget::Attachment(arrival) => Some(arrival.clone()),
                _ => None,
            })
            .collect()
    }

    /// Remove the attachment definition for store upload `name`, with every
    /// occurrence that pointed at it — what a client does when the operator
    /// removes a recalled turn's staged chip, so the held document cannot
    /// resubmit an upload that was taken back. Other definitions stay.
    pub fn detach_attachment(&mut self, name: &str) {
        self.references.retain(|_, definition| {
            !matches!(&definition.target, ReferenceTarget::Attachment(arrival) if arrival.name == name)
        });
        self.content.retain(|segment| match segment {
            UserSegment::Text(_) => true,
            UserSegment::Reference(id) => self.references.contains_key(id),
        });
    }

    /// The readable flat form (docs/control-api.md): prose verbatim, a
    /// filesystem chip as the path the submitter typed, an agent chip as
    /// `@session`, an attachment chip as nothing. An attachment is a stored
    /// upload, not prose: a caller that shows this to a person shows the
    /// upload's notice or name beside it (the TUI's staged chips, a
    /// transcript's definition lines). Re-typing any of it as text grants
    /// nothing, so drafts, recalls and status views show it safely.
    pub fn readable(&self) -> String {
        let mut out = String::new();
        for segment in &self.content {
            match segment {
                UserSegment::Text(text) => out.push_str(text),
                UserSegment::Reference(id) => {
                    if let Some(definition) = self.references.get(id) {
                        out.push_str(&definition.readable());
                    }
                }
            }
        }
        out
    }

    /// The document as the model reads it (docs/log-and-projection.md): ONE
    /// string — prose and `[id]` tokens concatenated verbatim, then a
    /// `references:` block listing every definition's readable metadata in id
    /// order. One string, not one part per piece, so no provider encoder
    /// interposes separators between the document's pieces. The source prefix
    /// follows a sourced `User` turn's rule; a document with no prose gives
    /// the source a line of its own.
    pub fn render(&self, source: Option<&str>) -> String {
        let mut prose = String::new();
        for segment in &self.content {
            match segment {
                UserSegment::Text(text) => prose.push_str(text),
                UserSegment::Reference(id) => {
                    prose.push('[');
                    prose.push_str(id);
                    prose.push(']');
                }
            }
        }
        let has_prose = !prose.trim().is_empty();
        let body = if self.references.is_empty() {
            prose
        } else {
            let mut block = String::from("references:");
            for (id, definition) in &self.references {
                block.push_str(&format!("\n[{id}] {}", definition.describe_line()));
            }
            if has_prose {
                format!("{prose}\n\n{block}")
            } else {
                block
            }
        };
        match source.map(sources::wire_label) {
            Some(label) if has_prose => format!("{label}: {body}"),
            Some(label) => format!("{label}:\n{body}"),
            None => body,
        }
    }
}
