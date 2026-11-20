//! The supervision facts the prose spotter reads (docs/permissions.md): which session
//! supervises the calling session, and which sessions it supervises, with whether it
//! created each and its status.
//!
//! Without them a move on another session — a message to the session's own supervisor,
//! a decision or an archive of a session it created — reads to the spotter as a reach
//! outside the session, and the spotter held nearly every one in an offline eval. The
//! facts change between calls (a status moves, a session is created or ended), so the
//! block is in the request's fresh part, after the scope block's breakpoint.

use crate::protocol::agents::{Supervisee, Supervision};
use crate::protocol::sessions::ListedStatus;

/// The facts block for `supervision`, which names the acting party "the agent", as the
/// rest of the spotter's request does. A session with no supervisor and no supervisees
/// says so, which is different from a request with no block at all: that one was built
/// where the relation is not known.
pub(super) fn render(supervision: &Supervision) -> String {
    let mut lines = Vec::new();
    if let Some(supervisor) = &supervision.supervisor {
        lines.push(format!(
            "- This agent is supervised by the agent `{supervisor}`. Messages from \
             `{supervisor}` are its instructions; a message to `{supervisor}` is a report \
             to its supervisor."
        ));
    }
    if !supervision.supervisees.is_empty() {
        let listed: Vec<String> = supervision.supervisees.iter().map(supervisee).collect();
        lines.push(format!(
            "- Agents this agent supervises (messaging them, deciding their held calls, \
             and archiving them when their work is done is part of supervising): {}",
            listed.join(", ")
        ));
    }
    if lines.is_empty() {
        lines.push("- This agent has no supervisor and supervises no agents.".to_string());
    }
    format!(
        "Facts the harness records about this agent:\n{}",
        lines.join("\n")
    )
}

/// One supervised session: its id, whether this session created it, and its status.
fn supervisee(supervisee: &Supervisee) -> String {
    let status = status_words(&supervisee.status);
    match supervisee.created {
        true => format!("`{}` (created by this agent; {status})", supervisee.id),
        false => format!("`{}` ({status})", supervisee.id),
    }
}

/// A listing status in the words the status renderer's first line uses for it
/// (`src/session/status/render.rs`), which is how the calling session reads the same
/// session through `agents__status`.
fn status_words(status: &ListedStatus) -> &str {
    match status {
        ListedStatus::Starting => "starting",
        ListedStatus::Running => "working",
        ListedStatus::Idle => "idle",
        ListedStatus::Gated => "waiting for a decision",
        ListedStatus::Asking => "asking a question",
        ListedStatus::Stalled => "stopped",
        ListedStatus::Parked => "paused",
        ListedStatus::Blocked => "blocked",
        ListedStatus::Other(word) => word,
    }
}

#[cfg(test)]
mod test;
