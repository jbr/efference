//! The conversation block the prose spotter reads (docs/permissions.md): every utterance
//! of the session, whole and oldest first, each between an open and a close tag naming
//! its writer.
//!
//! The block sits between the request's two breakpoints, so it is part of the cached
//! prefix, and it is rendered so that a later call's block starts with an earlier call's
//! block byte for byte: each utterance is a part of its own, which keeps the block
//! boundaries of an earlier rendering in place in a later one, and nothing in a part
//! depends on what comes after it.

use crate::record::utterances::{Speaker, Utterance, Utterances};

/// The block's heading, its first part.
const HEADING: &str = "The conversation in this session, oldest first:\n\n";

/// The part a session with no utterances renders after the heading.
const NOTHING: &str = "(nothing before this move)\n\n";

/// The tag names a writer's message is wrapped in. Text inside a message that starts one
/// of them is escaped ([`escaped`]).
const TAG_NAMES: [&str; 3] = ["operator", "agent-text", "supervisor"];

/// What replaces the `<` that would open or close a tag inside a message's text.
const ESCAPED_ANGLE: char = '‹';

/// The block as parts, heading first. A pure function of `utterances`, so an identical
/// log renders identical bytes, a host restart included.
pub(super) fn parts(utterances: &Utterances) -> Vec<String> {
    let mut parts = vec![HEADING.to_string()];
    if utterances.said.is_empty() {
        parts.push(NOTHING.to_string());
    }
    parts.extend(
        utterances
            .said
            .iter()
            .map(|utterance| render(utterance, &utterances.id)),
    );
    parts
}

/// One utterance as its part: its text between an open and a close tag that name its
/// writer and carry the session's tag id.
fn render(utterance: &Utterance, id: &str) -> String {
    let tag = match &utterance.speaker {
        Speaker::Operator => format!("operator id=\"{id}\""),
        Speaker::Supervisor(agent) => format!("supervisor agent=\"{agent}\" id=\"{id}\""),
        Speaker::Agent => format!("agent-text id=\"{id}\""),
    };
    format!("<{tag}>\n{}\n</{tag}>\n\n", escaped(&utterance.text))
}

/// `text` with every `<` that starts one of the [`TAG_NAMES`], opening or closing and in
/// any letter case, replaced by [`ESCAPED_ANGLE`], so nothing a message contains reads
/// as the end of that message or the start of another. The rest of the text is
/// unchanged.
fn escaped(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find('<') {
        out.push_str(&rest[..at]);
        let after = &rest[at + 1..];
        let name = after.strip_prefix('/').unwrap_or(after);
        let opens_a_tag = TAG_NAMES.iter().any(|tag| {
            name.get(..tag.len())
                .is_some_and(|head| head.eq_ignore_ascii_case(tag))
        });
        out.push(if opens_a_tag { ESCAPED_ANGLE } else { '<' });
        rest = after;
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod test;
