//! Classifying turn endings — was stopping worth the round trip?
//! (docs/operating.md; read by `efference dev stops`.)
//!
//! A turn ends when the model emits no tool call, and the loop yields to whoever
//! sends the next one. That is the harness's only way to put a question *and* its
//! only way to say the work is done, so the same action carries three meanings and
//! the log cannot ask which was intended. What it can do is look at what happened
//! next: **a stop that the next turn answered with no information bought nothing.**
//!
//! That is the measurable half of the three licensed stops. A [`Stop::Barren`] is
//! the failure — the session paused, the operator said "carry on", and the only
//! thing the round trip produced was latency and a cache write. The other arms are
//! not graded here: a [`Stop::Question`] may be a good question or a needless one,
//! and a [`Stop::Answered`] may be a welcome checkpoint or a missed chance to keep
//! going. Those need a task whose answer is known in advance (eval/harness/README.md);
//! this fold needs only the log.

use super::{LogEntry, Payload};

/// What a turn ending turned out to be, judged by the turn that followed it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stop {
    /// The final message asked something. Whether it needed asking is not a
    /// question the log can settle, so this arm is counted, never faulted.
    Question,
    /// Nothing followed. The session ended here — finished, or abandoned; the log
    /// does not distinguish them and neither does this.
    Final,
    /// The next turn carried instruction, correction, or an answer. The stop
    /// yielded something, whatever it cost.
    Answered,
    /// The next turn carried no information: an acknowledgement, a "carry on", an
    /// emoji. Nothing was asked and nothing was supplied, so the round trip was
    /// spent on nothing.
    Barren,
}

/// Every turn ending in one session's log, in order, each paired with the label of
/// whoever answered it — `None` for the operator, `agent[<id>]` / `mcp[<name>]` /
/// `web` otherwise (`LogEntry::source`). A barren stop answered by a supervising
/// agent is the same waste as one answered by a person, and worth telling apart.
pub fn classify(entries: &[LogEntry]) -> Vec<(Stop, Option<String>)> {
    let mut stops = Vec::new();
    let mut pending = false;
    for entry in entries {
        match &entry.payload {
            // A record-only entry is a failed request, not something the model said:
            // it ended nothing, and a reply to it answered the failure, not a stop.
            Payload::Assistant {
                upstream_failure: Some(_),
                ..
            } => {}
            Payload::Assistant { output, .. } => {
                // A call still to run means the loop continues — including a call
                // held at a permission gate, which is a pause, not an ending.
                let ended = output.tool_calls().map(|c| c.is_empty()).unwrap_or(false);
                if ended {
                    pending = true;
                    if asks(&output.text()) {
                        stops.push((Stop::Question, None));
                        pending = false;
                    }
                }
            }
            Payload::User(blocks) if pending => {
                let stop = if informative(blocks) {
                    Stop::Answered
                } else {
                    Stop::Barren
                };
                stops.push((stop, entry.source.clone()));
                pending = false;
            }
            _ => {}
        }
    }
    if pending {
        stops.push((Stop::Final, None));
    }
    stops
}

/// Whether the assistant's parting prose put a question. A trailing `?` is the
/// signal: a question mark mid-paragraph is usually rhetorical or quoted, and one
/// at the end of the last thing said is the model handing the turn over on it.
fn asks(text: &[&str]) -> bool {
    text.iter()
        .rev()
        .find_map(|block| {
            let trimmed = block.trim_end();
            (!trimmed.is_empty()).then_some(trimmed.ends_with('?'))
        })
        .unwrap_or(false)
}

/// Whether a user turn carried anything the session did not already have.
///
/// The test is deliberately generous toward "informative": every word must be a
/// pure acknowledgement for the turn to count as barren, so "carry on" is barren
/// and "carry on, but commit first" is not. Over-counting [`Stop::Answered`]
/// understates the problem, which is the safe direction for a number meant to
/// justify changing the preamble.
fn informative(blocks: &[super::ContentBlock]) -> bool {
    for block in blocks {
        let super::ContentBlock::Text(text) = block else {
            continue;
        };
        // Who sent a turn rides on `LogEntry::source`, not in the prose, so the
        // text here is the sender's own words with no prefix to strip.
        for word in text.split(|c: char| !c.is_alphanumeric() && c != '\'') {
            let word = word.trim_matches('\'').to_ascii_lowercase();
            if word.is_empty() {
                continue;
            }
            if !ACKNOWLEDGEMENT.contains(&word.as_str()) {
                return true;
            }
        }
    }
    // Falling through means every word was an acknowledgement — or there were no
    // words at all, which is the emoji case: the splitter treats an emoji as a
    // separator, so a turn of nothing but reactions reaches here with none.
    false
}

/// Words that, alone or together, tell a session nothing it did not already know.
/// Kept small and literal on purpose: every addition widens what counts as barren,
/// so the list holds only words that cannot carry instruction in any arrangement.
/// Deliberately absent are "stop", "wait", "no", "not", "don't" and "instead" —
/// short, but each one reverses a decision.
const ACKNOWLEDGEMENT: [&str; 43] = [
    "ack",
    "agreed",
    "ahead",
    "all",
    "along",
    "awesome",
    "brilliant",
    "carry",
    "continue",
    "cool",
    "excellent",
    "go",
    "going",
    "good",
    "great",
    "k",
    "keep",
    "lgtm",
    "lovely",
    "mhm",
    "neat",
    "nice",
    "now",
    "ok",
    "okay",
    "on",
    "onward",
    "perfect",
    "please",
    "proceed",
    "right",
    "sounds",
    "sure",
    "sweet",
    "thank",
    "thanks",
    "thx",
    "up",
    "with",
    "yeah",
    "yep",
    "yes",
    "yup",
];

#[cfg(test)]
mod test;
