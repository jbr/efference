//! The digest view of a triage request (backlog: triage-reads-the-digest.md): the
//! history as the trajectory digest renders it (docs/digest.md) instead of as the
//! agent's transcript. An experiment arm of `efference dev peek`; the live gate reads the
//! transcript view.
//!
//! The request carries no assistant turn and no tool block, only text, so it encodes on
//! any dialect: the gate's calls are written out with their arguments, and their results
//! follow as the transcript view shows them.

use super::prompt::gate_parts;
use super::*;
use std::fmt::Write as _;

/// Which rendering of the history a triage request reads.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum TriageView {
    /// The agent's transcript, as the main wire projects it (the live gate's view).
    #[default]
    Transcript,
    /// The trajectory digest: one line per call with its intent and note, the operator's
    /// turns and the agent's prose.
    Digest,
}

const DIGEST_HEAD: &str = "The session so far, as a digest: each operator turn, the \
agent's prose, and one line per tool call with its intent and the note triage left on \
its result.";

const GATE_HEAD: &str = "The agent's latest calls, and what each returned:";

/// The digest view's request: one user turn holding the digest of everything before the
/// gate's results, a breakpoint after it (the digest of a longer log extends a shorter
/// one's, so this is where a cache can resume), then the gate's calls and results and
/// the same ask the transcript view sends.
pub(super) fn digest_peek_messages(
    prefix: &[LogEntry],
    reader: Reader,
    pending: &[Pending<'_>],
    inbox: &[ShownNotification],
    background: &[BgView],
    roster: &[Item],
) -> Vec<WireTurn> {
    let digest = crate::digest::render(
        prefix,
        crate::digest::Options {
            reader,
            ..Default::default()
        },
    );
    let (results, ask) = gate_parts(pending, inbox, background, roster);
    let mut gate = format!("{GATE_HEAD}\n");
    for (p, result) in pending.iter().zip(results) {
        let _ = writeln!(gate, "\n• {} {}", p.call.name, p.call.input);
        if result.is_error {
            let _ = writeln!(gate, "(error)");
        }
        let _ = writeln!(gate, "{}", result.content);
    }
    vec![WireTurn::User(vec![
        UserPart::Text(format!("{DIGEST_HEAD}\n\n{}", digest.trim_end())),
        UserPart::Breakpoint,
        UserPart::Text(gate),
        UserPart::Text(ask),
    ])]
}

#[cfg(test)]
mod test;
