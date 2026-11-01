//! The inline fenced artifact syntax (docs/artifacts.md): a fenced code block whose
//! info string is exactly `html-artifact` or `mermaid`, in committed assistant text.
//!
//! An artifact's coordinates are its text block's ordinal among the entry's text
//! blocks and its ordinal among that block's artifact fences, of either kind. Fences
//! are found by CommonMark's rules alone — inside any container, and running to the
//! container's end when nothing closes them — so a client numbering the same blocks
//! with its own CommonMark parser (the web client's remark) arrives at the same
//! ordinals. `web/src/ui/artifact-fences.json` holds the cases both sides are tested
//! against.

use pulldown_cmark::{CodeBlockKind, Event, Options, Parser, Tag, TagEnd};

/// What a fence draws, named by its info string.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[cfg_attr(test, derive(schemars::JsonSchema), schemars(rename = "ArtifactKind"))]
#[serde(rename_all = "lowercase")]
pub(crate) enum Kind {
    /// `html-artifact`: a page the agent wrote.
    Html,
    /// `mermaid`: a Mermaid diagram, which the host draws in a page of its own.
    Mermaid,
}

impl Kind {
    const ALL: [Kind; 2] = [Kind::Html, Kind::Mermaid];

    /// The info string that opts a fenced block into rendering as this kind.
    pub(crate) fn info(self) -> &'static str {
        match self {
            Kind::Html => "html-artifact",
            Kind::Mermaid => "mermaid",
        }
    }

    fn of(info: &str) -> Option<Kind> {
        Kind::ALL.into_iter().find(|kind| kind.info() == info)
    }
}

/// One artifact fence: its kind and its source.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Fence {
    pub(crate) kind: Kind,
    pub(crate) source: String,
}

/// Each artifact fence in `markdown`, in order.
pub(crate) fn fences(markdown: &str) -> Vec<Fence> {
    // Most text carries no artifact; skip the parse for it.
    if !Kind::ALL.iter().any(|kind| markdown.contains(kind.info())) {
        return Vec::new();
    }
    let mut fences = Vec::new();
    let mut current: Option<Fence> = None;
    for event in Parser::new_ext(markdown, gfm()) {
        match event {
            Event::Start(Tag::CodeBlock(CodeBlockKind::Fenced(info))) => {
                current = Kind::of(&info).map(|kind| Fence {
                    kind,
                    source: String::new(),
                });
            }
            Event::Text(text) => {
                if let Some(fence) = &mut current {
                    fence.source.push_str(&text);
                }
            }
            Event::End(TagEnd::CodeBlock) => fences.extend(current.take()),
            _ => {}
        }
    }
    fences
}

/// The extensions remark-gfm parses, so both renderers see the same blocks.
fn gfm() -> Options {
    Options::ENABLE_TABLES
        | Options::ENABLE_FOOTNOTES
        | Options::ENABLE_STRIKETHROUGH
        | Options::ENABLE_TASKLISTS
}

#[cfg(test)]
mod test;
