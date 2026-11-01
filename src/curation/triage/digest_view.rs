//! Which rendering of the history a triage request reads (docs/triage.md): the agent's
//! transcript, the text view a separate triage model reads (`text_view.rs`), or one of
//! the text-only arms `efference dev triage --view` replays against it.
//!
//! The text-only requests carry no assistant turn and no tool block, so they encode on
//! any dialect: the triage's calls are written out with their arguments, and their
//! results follow as the transcript view shows them.

use super::text_view::{current_turn, history_end, text_triage_messages};
use super::*;

/// Which rendering of the history a triage request reads.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum TriageView {
    /// The agent's transcript, as the main wire projects it: the shared triage's view.
    #[default]
    Transcript,
    /// The text view (docs/triage.md): summaries for the triage reader, the digest up
    /// to the cut, the triaged tail in full, then the current step. What a separate
    /// triage model reads.
    Text,
    /// An arm: the digest of only the last `n` assistant turns, the triage's own
    /// counted, as the whole history.
    Tail(usize),
    /// An arm: no history, only the triage's calls, their results, and the ask.
    Intent,
}

impl TriageView {
    /// `transcript`, `text`, `tail:N`, or `intent`.
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "transcript" => Some(Self::Transcript),
            "text" => Some(Self::Text),
            "intent" => Some(Self::Intent),
            _ => s
                .strip_prefix("tail:")
                .and_then(|n| n.parse().ok())
                .filter(|n| *n > 0)
                .map(Self::Tail),
        }
    }
}

const TAIL_HEAD: &str = "The session's most recent turns, as a digest: the agent's \
prose, and one line per tool call with its intent and the note triage left on its result.";

/// A text-only view's request: the history `view` asks for as its own user turn, ending
/// on a breakpoint, then the current step and the same ask the text view sends.
pub(super) fn text_only_messages(
    prefix: &[LogEntry],
    reader: Reader,
    view: TriageView,
    pending: &[Pending<'_>],
    inbox: &[ShownNotification],
    background: &[BgView],
    roster: &[Item],
) -> Vec<WireTurn> {
    let end = history_end(prefix, pending.first().map(|p| p.call));
    let history = match view {
        TriageView::Text => {
            return text_triage_messages(prefix, reader, pending, inbox, background, roster);
        }
        TriageView::Intent | TriageView::Transcript => None,
        TriageView::Tail(n) => {
            let start = prefix[..end]
                .iter()
                .enumerate()
                .filter(|(_, e)| matches!(e.payload, Payload::Assistant { .. }))
                .map(|(i, _)| i)
                .rev()
                .nth(n - 1)
                .unwrap_or(0);
            let digest = crate::curation::digest::render(
                &prefix[start..end],
                crate::curation::digest::Options {
                    reader,
                    ..Default::default()
                },
            );
            Some(WireTurn::User(vec![
                UserPart::Text(format!("{TAIL_HEAD}\n\n{}", digest.trim_end())),
                UserPart::Breakpoint,
            ]))
        }
    };
    history
        .into_iter()
        .chain([current_turn(
            prefix, end, reader, pending, inbox, background, roster,
        )])
        .collect()
}

#[cfg(test)]
mod test;
