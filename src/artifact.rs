//! The inline fenced artifact syntax (docs/artifacts.md): a fenced code block whose
//! info string is exactly `html-artifact`, in committed assistant text.
//!
//! An artifact's coordinates are its text block's ordinal among the entry's text
//! blocks and its ordinal among that block's artifact fences. Fences are found by
//! CommonMark's rules alone — inside any container, and running to the container's
//! end when nothing closes them — so a client numbering the same blocks with its own
//! CommonMark parser (the web client's remark) arrives at the same ordinals.
//! `web/src/ui/artifact-fences.json` holds the cases both sides are tested against.

use pulldown_cmark::{CodeBlockKind, Event, Options, Parser, Tag, TagEnd};

/// The info string that opts a fenced block into rendering.
pub(crate) const INFO: &str = "html-artifact";

/// The source of each `html-artifact` fence in `markdown`, in order.
pub(crate) fn html_artifact_fences(markdown: &str) -> Vec<String> {
    // Most text carries no artifact; skip the parse for it.
    if !markdown.contains(INFO) {
        return Vec::new();
    }
    let mut fences = Vec::new();
    let mut current: Option<String> = None;
    for event in Parser::new_ext(markdown, gfm()) {
        match event {
            Event::Start(Tag::CodeBlock(CodeBlockKind::Fenced(info))) if info.as_ref() == INFO => {
                current = Some(String::new());
            }
            Event::Text(text) => {
                if let Some(html) = &mut current {
                    html.push_str(&text);
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
