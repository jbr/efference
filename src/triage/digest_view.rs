//! The digest view of a triage request (backlog: separate-triage-reads-text.md): the
//! history as the trajectory digest renders it (docs/digest.md) instead of as the
//! agent's transcript. An experiment arm of `efference dev triage`; the live triage reads the
//! transcript view.
//!
//! The request carries no assistant turn and no tool block, only text, so it encodes on
//! any dialect: the triage's calls are written out with their arguments, and their results
//! follow as the transcript view shows them.

use super::prompt::triage_parts;
use super::*;
use std::fmt::Write as _;

/// Which rendering of the history a triage request reads.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum TriageView {
    /// The agent's transcript, as the main wire projects it (the live triage's view).
    #[default]
    Transcript,
    /// The trajectory digest: one line per call with its intent and note, the operator's
    /// turns and the agent's prose.
    Digest,
    /// The digest of only the last `n` assistant turns, the triage's own counted: a recent
    /// tail rather than the whole history.
    Tail(usize),
    /// No history: the triage's calls, their results, and the ask.
    Intent,
}

impl TriageView {
    /// `transcript`, `digest`, `tail:N`, or `intent`.
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "transcript" => Some(Self::Transcript),
            "digest" => Some(Self::Digest),
            "intent" => Some(Self::Intent),
            _ => s
                .strip_prefix("tail:")
                .and_then(|n| n.parse().ok())
                .filter(|n| *n > 0)
                .map(Self::Tail),
        }
    }
}

const DIGEST_HEAD: &str = "The session so far, as a digest: each operator turn, the \
agent's prose, and one line per tool call with its intent and the note triage left on \
its result.";

const TAIL_HEAD: &str = "The session's most recent turns, as a digest: the agent's \
prose, and one line per tool call with its intent and the note triage left on its result.";

const TRIAGE_HEAD: &str = "The agent's latest calls, and what each returned:";

/// The text-only views' request: one user turn holding the history `view` asks for (the
/// whole digest, a tail of it, or none), a breakpoint after it (the digest of a longer
/// log extends a shorter one's, so this is where a cache can resume), then the triage's
/// calls and results and the same ask the transcript view sends.
pub(super) fn digest_triage_messages(
    prefix: &[LogEntry],
    reader: Reader,
    view: TriageView,
    pending: &[Pending<'_>],
    inbox: &[ShownNotification],
    background: &[BgView],
    roster: &[Item],
) -> Vec<WireTurn> {
    let render = |entries: &[LogEntry]| {
        crate::digest::render(
            entries,
            crate::digest::Options {
                reader,
                ..Default::default()
            },
        )
    };
    let history = match view {
        TriageView::Intent | TriageView::Transcript => None,
        TriageView::Digest => Some(format!("{DIGEST_HEAD}\n\n{}", render(prefix).trim_end())),
        TriageView::Tail(n) => {
            let start = prefix
                .iter()
                .enumerate()
                .filter(|(_, e)| matches!(e.payload, Payload::Assistant { .. }))
                .map(|(i, _)| i)
                .rev()
                .nth(n - 1)
                .unwrap_or(0);
            Some(format!(
                "{TAIL_HEAD}\n\n{}",
                render(&prefix[start..]).trim_end()
            ))
        }
    };
    let (results, ask) = triage_parts(pending, inbox, background, roster, reader);
    let mut triage = format!("{TRIAGE_HEAD}\n");
    for (p, result) in pending.iter().zip(results) {
        let _ = writeln!(triage, "\n• {} {}", p.call.name, p.call.input);
        if result.is_error {
            let _ = writeln!(triage, "(error)");
        }
        let _ = writeln!(triage, "{}", result.content);
    }
    let mut parts = Vec::new();
    if let Some(history) = history {
        parts.push(UserPart::Text(history));
        parts.push(UserPart::Breakpoint);
    }
    parts.extend([UserPart::Text(triage), UserPart::Text(ask)]);
    vec![WireTurn::User(parts)]
}

#[cfg(test)]
mod test;
