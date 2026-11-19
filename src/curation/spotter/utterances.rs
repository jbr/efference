//! The utterances block the prose spotter reads (docs/permissions.md): what the operator,
//! and any session supervising this one, said in the session, oldest first, each with the
//! end of the session's text it answers.
//!
//! The block sits between the request's two breakpoints, so it is part of the cached
//! prefix, and it is rendered so that a later call's block starts with an earlier call's
//! block byte for byte. Each utterance is a part of its own, which keeps the block
//! boundaries of an earlier rendering in place in a later one. Past a size budget the
//! block rolls over once ([`parts`]): it starts again with the first utterance, a marker
//! counting the ones left out, and the most recent ones, and grows by appending from
//! there.

use crate::record::utterances::{Speaker, Utterance};

/// The block's heading, its first part.
const HEADING: &str = "What the operator has said in this session, oldest first:\n\n";

/// The part a session with no utterances renders after the heading.
const NOTHING: &str = "(nothing before this move)\n\n";

/// The most characters the utterance parts may hold together, marker included, before
/// the block rolls over.
pub(super) const BUDGET: usize = 24_000;

/// The block as parts, heading first. A pure function of `utterances`, so an identical
/// log renders identical bytes, a host restart included.
pub(super) fn parts(utterances: &[Utterance]) -> Vec<String> {
    let rendered: Vec<String> = utterances.iter().map(render).collect();
    let mut parts = vec![HEADING.to_string()];
    if rendered.is_empty() {
        parts.push(NOTHING.to_string());
    } else {
        parts.extend(rolled(&rendered));
    }
    parts
}

/// The utterance parts, `rendered` non-empty. Walking them in order, each is appended
/// until appending one would take the parts past [`BUDGET`]; then the block rolls over:
/// the first utterance, a marker counting the utterances left out, then the most recent
/// ones, the one being appended among them, that fit in half the budget. The walk goes on
/// appending from there.
fn rolled(rendered: &[String]) -> Vec<String> {
    let size = |part: &String| part.chars().count();
    let mut out = vec![rendered[0].clone()];
    let mut used = size(&rendered[0]);
    for (index, part) in rendered.iter().enumerate().skip(1) {
        if used + size(part) <= BUDGET {
            used += size(part);
            out.push(part.clone());
            continue;
        }
        // The most recent utterances that fit in half the budget, back from this one;
        // this one is kept whatever its size.
        let mut start = index;
        let mut kept = size(part);
        while start > 1 && kept + size(&rendered[start - 1]) <= BUDGET / 2 {
            start -= 1;
            kept += size(&rendered[start]);
        }
        out.truncate(1);
        used = size(&rendered[0]) + kept;
        let left_out = start - 1;
        if left_out > 0 {
            let marker = marker(left_out);
            used += size(&marker);
            out.push(marker);
        }
        out.extend(rendered[start..=index].iter().cloned());
    }
    out
}

/// The line that stands for the utterances a rollover left out.
fn marker(left_out: usize) -> String {
    format!("[… {left_out} earlier turns left out …]\n\n")
}

/// One utterance as its part: the end of the session's text it answers, when there is
/// one, then who said it and what they said.
fn render(utterance: &Utterance) -> String {
    let mut out = String::new();
    if let Some(answers) = &utterance.answers {
        out.push_str("[the session had just written]\n");
        out.push_str(answers);
        out.push('\n');
    }
    match &utterance.speaker {
        Speaker::Operator => out.push_str("Operator:\n"),
        Speaker::Session(id) => out.push_str(&format!("Supervising session `{id}`:\n")),
    }
    out.push_str(&utterance.text);
    out.push_str("\n\n");
    out
}

#[cfg(test)]
mod test;
