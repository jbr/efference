//! The text view (docs/triage.md, "The text view"): what a separate triage model reads
//! in place of the agent's transcript. Text only, so a triage model of any wire format
//! reads it, and laid out so that between view cuts each triage's view extends the last.
//!
//! The view is two user turns. The first is the **history**: the summaries written for
//! the triage reader, the trajectory digest (docs/digest.md) up to the **view cut**
//! (named apart from a compaction's cut, docs/summary.md), then the **triaged tail** —
//! every step after the view cut in full, each result as the agent read it. The second
//! is the current step: the agent's latest prose and calls with their raw output, and
//! the ask. "The cut" below is always the view cut.
//!
//! Everything here is a pure function of the log. The history is divided into
//! **steps**, each starting at an assistant entry that made calls; the cut only ever
//! falls at a step's start, and where it falls is decided by a fold over the steps'
//! sizes ([`TRIAGED_TAIL_HIGH_WATER`], [`TRIAGED_TAIL_LOW_WATER`]), so a replay
//! reproduces it and nothing needs recording.

use super::*;
use crate::digest::{Fold, Options};
use std::fmt::Write as _;

/// When the triaged tail's rendered size passes this many bytes, the cut advances
/// (docs/triage.md). Measured in bytes because what a cut buys is wire weight.
pub const TRIAGED_TAIL_HIGH_WATER: usize = 64 * 1024;

/// How much of the triaged tail a cut keeps in full: the most recent steps that fit
/// in this many bytes (docs/triage.md). The steps before them move into the digest.
pub const TRIAGED_TAIL_LOW_WATER: usize = 32 * 1024;

/// The history's opening block. It never changes, so it heads every cached prefix.
const HISTORY_HEAD: &str = "═══ record · the agent's session so far ═══\n\
A record of the agent's work up to its latest turn, written by the harness: the operator's \
turns, the agent's prose, and its tool calls. Earlier calls appear as a digest, one line per \
call with its intent and the note triage left on its result; the most recent calls follow in \
full, each with its result as the agent read it after triage.";

/// The head over a summary's account. A compaction's account is addressed to the agent
/// (docs/summary.md), so the head says whom its "you" means.
const SUMMARY_HEAD: &str =
    "─── summary of the session before the digest, addressed to the agent as \"you\" ───";

/// The head of the first digest block.
const DIGEST_HEAD: &str = "─── digest of earlier calls ───";

/// The head over the triaged tail.
const TAIL_HEAD: &str = "─── recent calls in full ───";

/// The current step's head, opening the second turn.
const CURRENT_HEAD: &str = "═══ the agent's latest turn ═══\n\
Its prose, then each call it made with the full output, as triage is deciding it:";

/// The history half of the view, laid out for one triage.
pub(super) struct History {
    /// The turn's parts: text blocks, with a breakpoint at the end of the digest and
    /// another at the end of the triaged tail — the end of the history so far.
    pub(super) parts: Vec<UserPart>,
    /// The seq of the entry each cut moved the triaged tail's start to, in order.
    pub(super) cuts: Vec<Seq>,
    /// The rendered size of the digest blocks, in bytes.
    pub(super) digest_bytes: usize,
    /// The rendered size of the triaged tail, in bytes.
    pub(super) tail_bytes: usize,
}

impl History {
    /// The history as a user turn, ending on its breakpoint.
    pub(super) fn turn(self) -> WireTurn {
        WireTurn::User(self.parts)
    }
}

/// Whether `entry` starts a step: an assistant turn that made calls.
fn starts_step(entry: &LogEntry) -> bool {
    match &entry.payload {
        Payload::Assistant {
            output,
            upstream_failure: None,
        } => output.tool_calls().is_ok_and(|calls| !calls.is_empty()),
        _ => false,
    }
}

/// The index of the assistant entry that made `call`: where the history of the triage
/// covering it ends. A live triage's log already holds the batch's captured results
/// after it, and those belong to the current step, not the history. With no call (a
/// title ask, docs/session-title.md), the whole log.
pub(super) fn history_end(entries: &[LogEntry], call: Option<&ToolCall>) -> usize {
    let Some(call) = call else {
        return entries.len();
    };
    entries
        .iter()
        .rposition(|e| match &e.payload {
            Payload::Assistant { output, .. } => output
                .tool_calls()
                .is_ok_and(|calls| calls.iter().any(|c| c.id == call.id)),
            _ => false,
        })
        .unwrap_or(entries.len())
}

/// [`history_end`] for the triage of `pending`.
fn pending_end(entries: &[LogEntry], pending: &[Pending<'_>]) -> usize {
    history_end(entries, pending.first().map(|p| p.call))
}

/// Lay out the history of `entries[..end]` for `reader` (docs/triage.md).
///
/// Head summaries addressed to `reader` lead, and the entries they cover are left out.
/// The rest is split into steps; the cut fold walks the step starts in order, and
/// wherever the triaged tail (the steps since the cut, rendered in full) has passed
/// [`TRIAGED_TAIL_HIGH_WATER`], it moves the cut forward to the earliest step start
/// that leaves at most [`TRIAGED_TAIL_LOW_WATER`] after it. Each stretch a cut moved
/// into the digest is one block, so an earlier digest's blocks are a prefix of a later
/// one's; each step in the triaged tail is one block, so an earlier triage's history
/// ends on a block boundary of a later one's. A provider that matches cached prefixes
/// only at block boundaries (Anthropic) needs both.
pub(super) fn history(log: &[LogEntry], end: usize, reader: Reader) -> History {
    let entries = &log[..end];

    // Head summaries for this reader, in log order, and the last seq they cover.
    let heads: Vec<&crate::log::Summary> = entries
        .iter()
        .filter_map(|e| match &e.payload {
            Payload::Summary(s) if s.from == 0 && reader.applies(s.audience) => Some(s),
            _ => None,
        })
        .collect();
    let covered = heads.iter().map(|s| s.upto).max();
    let mut fold = Fold::new(entries);
    fold.hide(entries.iter().filter_map(|e| {
        let head = matches!(&e.payload, Payload::Summary(s)
            if s.from == 0 && reader.applies(s.audience));
        (head || covered.is_some_and(|c| e.seq <= c)).then_some(e.seq)
    }));

    // Step starts, with the history's own start and end as the outer bounds.
    let mut points = vec![0];
    points.extend((1..entries.len()).filter(|&i| starts_step(&entries[i])));
    points.push(entries.len());
    points.dedup();
    let spans: Vec<std::ops::Range<usize>> = points.windows(2).map(|w| w[0]..w[1]).collect();
    let options = Options {
        reader,
        ..Options::default()
    };
    let full: Vec<String> = spans
        .iter()
        .map(|span| {
            fold.render(
                span.clone(),
                Options {
                    verbatim: true,
                    ..options
                },
            )
        })
        .collect();

    // The cut fold: one check per step start, in order.
    let mut tail_from = 0;
    let mut cuts_at: Vec<usize> = Vec::new();
    for boundary in 1..=spans.len() {
        let mut size: usize = full[tail_from..boundary].iter().map(String::len).sum();
        if size > TRIAGED_TAIL_HIGH_WATER {
            while size > TRIAGED_TAIL_LOW_WATER {
                size -= full[tail_from].len();
                tail_from += 1;
            }
            cuts_at.push(tail_from);
        }
    }

    let mut parts = Vec::new();
    let mut head = HISTORY_HEAD.to_string();
    for summary in &heads {
        head.push_str("\n\n");
        head.push_str(SUMMARY_HEAD);
        head.push('\n');
        head.push_str(crate::compact::account_of(&summary.text));
    }
    head.push_str("\n\n");
    parts.push(UserPart::Text(head));

    // One block per stretch a cut moved; the first that renders anything carries the
    // digest's head. Blocks before it rendered nothing, and never will.
    let mut digest_bytes = 0;
    let mut start = 0;
    for &cut in &cuts_at {
        let mut block: String = spans[start..cut]
            .iter()
            .map(|span| fold.render(span.clone(), options))
            .collect();
        start = cut;
        if block.trim().is_empty() {
            continue;
        }
        if digest_bytes == 0 {
            block.insert_str(0, &format!("{DIGEST_HEAD}\n"));
        }
        digest_bytes += block.len();
        parts.push(UserPart::Text(block));
    }
    // A step that renders nothing gets no block: the Messages API refuses an empty or
    // whitespace-only text block.
    let tail: Vec<&String> = full[tail_from..]
        .iter()
        .filter(|t| !t.trim().is_empty())
        .collect();
    if digest_bytes > 0 && !tail.is_empty() {
        parts.push(UserPart::Breakpoint);
    }
    let tail_bytes = tail.iter().map(|t| t.len()).sum();
    if !tail.is_empty() {
        parts.push(UserPart::Text(format!("{TAIL_HEAD}\n")));
        parts.extend(tail.into_iter().map(|t| UserPart::Text(t.clone())));
    }
    parts.push(UserPart::Breakpoint);

    History {
        parts,
        // A cut that emptied the triaged tail falls at the history's end: the entry
        // there, the triage's own turn, is where the tail starts again.
        cuts: cuts_at
            .into_iter()
            .filter_map(|i| log.get(points[i]).map(|e| e.seq))
            .collect(),
        digest_bytes,
        tail_bytes,
    }
}

/// The current step's turn: the agent's latest prose and calls with their raw output,
/// then the ask (and the riders after it, docs/triage.md).
pub(super) fn current_turn(
    entries: &[LogEntry],
    end: usize,
    reader: Reader,
    pending: &[Pending<'_>],
    inbox: &[ShownNotification],
    background: &[BgView],
    roster: &[Item],
) -> WireTurn {
    let mut text = format!("{CURRENT_HEAD}\n\n");
    if end < entries.len() {
        let prose = Fold::new(&entries[..=end]).render(
            end..end + 1,
            Options {
                reader,
                ..Options::default()
            },
        );
        text.push_str(&prose);
    }
    let (results, ask) = triage_parts(
        pending,
        inbox,
        background,
        roster,
        reader,
        Framing::ThirdPerson,
    );
    for (p, result) in pending.iter().zip(results) {
        let _ = writeln!(text, "• {} {}", p.call.name, p.call.input);
        if result.is_error {
            let _ = writeln!(text, "(error)");
        }
        let _ = writeln!(text, "{}\n", result.content);
    }
    WireTurn::User(vec![
        UserPart::Text(text.trim_end().to_string()),
        UserPart::Text(ask),
    ])
}

/// The text view's whole request: the history through the step that made the pending
/// calls, then the current step and the ask.
pub(super) fn text_triage_messages(
    entries: &[LogEntry],
    reader: Reader,
    pending: &[Pending<'_>],
    inbox: &[ShownNotification],
    background: &[BgView],
    roster: &[Item],
) -> Vec<WireTurn> {
    let end = pending_end(entries, pending);
    vec![
        history(entries, end, reader).turn(),
        current_turn(entries, end, reader, pending, inbox, background, roster),
    ]
}

/// Where the text view stands at one triage (docs/triage.md), for the replay tool's
/// report: every cut the log has made so far and the sizes the history holds now.
pub struct TextViewLayout {
    /// The seq of the entry each cut moved the triaged tail's start to, in order.
    pub cuts: Vec<Seq>,
    /// The rendered digest, in bytes.
    pub digest_bytes: usize,
    /// The rendered triaged tail, in bytes.
    pub tail_bytes: usize,
}

/// The text view's layout at the triage of `pending` over `entries`.
pub fn text_view_layout(
    entries: &[LogEntry],
    pending: &[Pending<'_>],
    reader: Reader,
) -> TextViewLayout {
    let History {
        cuts,
        digest_bytes,
        tail_bytes,
        ..
    } = history(entries, pending_end(entries, pending), reader);
    TextViewLayout {
        cuts,
        digest_bytes,
        tail_bytes,
    }
}

#[cfg(test)]
mod test;
