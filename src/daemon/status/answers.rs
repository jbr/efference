//! What the session-control verbs answer (docs/tools.md), in the status's register:
//! the first line says what happened to which session, later lines open with a label,
//! and senders are plain names. One renderer per answer, shared by a session's own
//! toolbox, `efference mcp`, and the command line, each reading the status the answer
//! describes from wherever it can.

use super::render::{Reader, Relation, activity_text, duration, render};
use super::view::*;
use crate::daemon::Landing;
use crate::daemon::transcript::code_span;

/// The first line of `view`, as `reader` reads it.
pub(crate) fn first_line(view: &StatusView, reader: &Reader, now: u64) -> String {
    render(view, reader, now)
        .lines()
        .next()
        .unwrap_or_default()
        .to_string()
}

/// A held call by tool and what it does: ``bash `curl -sI …` ``.
fn held_call(held: &HeldCall) -> String {
    match &held.summary {
        Some(summary) => format!("{} {}", held.tool, code_span(summary)),
        None => held.tool.clone(),
    }
}

/// What a driver's message did, given the session's status as the message was sent.
pub(crate) fn sent(id: &str, landing: Landing, view: Option<&StatusView>, now: u64) -> String {
    let condition = view.map(|view| &view.condition);
    match landing {
        Landing::Answered => match condition {
            Some(Condition::Asking {
                question, handle, ..
            }) => {
                let handle = handle
                    .as_deref()
                    .map(|handle| format!(" {handle}"))
                    .unwrap_or_default();
                format!(
                    "answered: {id} has your answer to{handle} {:?} and goes on",
                    question.trim()
                )
            }
            _ => format!("answered: {id} has your answer and goes on"),
        },
        Landing::Now => format!("delivered: {id} is working on it"),
        Landing::Held => format!(
            "held: {id} is held by its operator; it reads this when the operator releases the hold"
        ),
        Landing::Queued => match condition {
            Some(Condition::Working { activity }) if !activity.is_empty() => format!(
                "queued: {id} reads this after its current step ({})",
                activity_text(&activity[0], now)
            ),
            Some(Condition::Gated(held)) => {
                let handle = held
                    .handle
                    .as_deref()
                    .map(|handle| format!("{handle} "))
                    .unwrap_or_default();
                format!(
                    "queued: {id} is waiting for a decision on {handle}({}); it reads this after \
                     the decision",
                    held_call(held)
                )
            }
            Some(Condition::Starting { .. }) => {
                format!("queued: {id} is starting; it reads this as its first message")
            }
            Some(Condition::Blocked {
                block: Block::PaceHeld,
            }) => format!(
                "queued: {id} is held by its operator; it reads this when the hold is released"
            ),
            Some(Condition::Blocked {
                block: Block::ConfigRepair { .. },
            }) => format!(
                "queued: {id} is blocked until the operator repairs its config; it reads this \
                 after that"
            ),
            _ => format!("queued: {id} reads this at its next step"),
        },
    }
}

/// A message from a session that does not drive its target.
pub(crate) fn left_in_inbox(id: &str) -> String {
    format!("left in {id}'s inbox; it may set it aside (you don't drive it)")
}

/// An empty message that the continue accepted.
pub(crate) fn continuing(id: &str) -> String {
    format!("continuing: {id} goes on from where it stopped")
}

/// An empty message from a session that does not drive its target.
pub(crate) fn empty_from_a_peer(id: &str) -> String {
    format!(
        "not delivered: an empty message continues a session you drive, and you don't drive \
         {id}; send it text to leave a note in its inbox"
    )
}

/// A reply from a session that does not drive its target.
pub(crate) fn reply_from_a_peer(id: &str) -> String {
    format!(
        "not delivered: only a session that drives {id} answers its questions; leave out \
         reply_to to put this in its inbox"
    )
}

/// What a gate decision did. `held` is the call as the status showed it before.
pub(crate) fn decided(id: &str, held: &HeldCall, allow: bool, note: Option<&str>) -> String {
    match (allow, note) {
        (true, None) => format!("allowed: {id} runs {}", held_call(held)),
        (true, Some(_)) => format!(
            "allowed with your note: {id} runs {}; your note arrives with its result",
            held_call(held)
        ),
        (false, Some(_)) => format!("denied: {id} continues under your message"),
        (false, None) => format!("denied: {id}'s turn stopped"),
    }
}

/// A decision naming a call that is not the pending gate: what the session is waiting
/// on now, from its status.
pub(crate) fn gate_moved(seq: &str, view: &StatusView, reader: &Reader, now: u64) -> String {
    format!(
        "gate moved: {seq} is not waiting for a decision; now {}",
        first_line(view, reader, now)
    )
}

/// What an interrupt stops, read from the status just before it is signalled, and
/// whether there is anything to signal.
pub(crate) fn interrupted(view: &StatusView, now: u64) -> (String, bool) {
    let id = view.id.as_str();
    match &view.condition {
        Condition::Working { activity } => match activity.first() {
            Some(Activity::Tool {
                tool,
                command,
                since,
            }) => {
                let call = match command {
                    Some(command) => format!("{tool} {}", code_span(command)),
                    None => tool.clone(),
                };
                let after = duration(now.saturating_sub(*since));
                (
                    format!(
                        "interrupted {id}: stopped {call} after {after}; its turn ends, and its \
                         next turn reads that you interrupted it"
                    ),
                    true,
                )
            }
            Some(Activity::Thinking { .. }) => (
                format!("interrupted {id}: stopped its generation; it is idle"),
                true,
            ),
            Some(other) => (
                format!(
                    "interrupted {id}: stopped it while {}",
                    activity_text(other, now)
                ),
                true,
            ),
            None => (format!("interrupted {id}"), true),
        },
        Condition::Idle { .. } | Condition::Stalled { .. } => {
            (format!("nothing to interrupt: {id} is idle"), false)
        }
        Condition::Ended | Condition::Failed { .. } | Condition::FailedToStart { .. } => {
            (format!("nothing to interrupt: {id} has ended"), false)
        }
        _ => {
            let now_line = first_line(
                view,
                &Reader::read(Relation::Driver, &super::Moves::MCP, None),
                now,
            );
            let doing = now_line
                .strip_prefix(&format!("{id}: "))
                .unwrap_or(&now_line)
                .to_string();
            (format!("interrupted {id}: it was {doing}"), true)
        }
    }
}

/// An archive, naming the sessions that ended with it.
pub(crate) fn archived(id: &str, ended_with: &[String]) -> String {
    match ended_with {
        [] => format!("archived {id}; its log stays on disk"),
        [one] => format!("archived {id} and the session it created ({one}); logs stay on disk"),
        many => format!(
            "archived {id} and the {} sessions it created ({}); logs stay on disk",
            many.len(),
            many.join(", ")
        ),
    }
}

/// A created session, as `create` answers it.
pub(crate) struct Created<'a> {
    pub(crate) id: &'a str,
    /// `<connection>/<model>`, when the session reports one.
    pub(crate) model: Option<String>,
    /// Where it works.
    pub(crate) working: &'a str,
    /// The project, when it works somewhere else: a worktree.
    pub(crate) project: Option<&'a str>,
    /// Its branch, when the worktree is its own.
    pub(crate) branch: Option<String>,
    /// Where its gates go, as a clause: `its gates come to you`.
    pub(crate) gates: String,
    /// Whether the creating mount watches it for `wait` (docs/operating.md).
    pub(crate) watched: bool,
    /// Its status stream's line over `efference mcp`: [`follow`], or [`no_stream`].
    pub(crate) follow: Option<String>,
    /// Its status's first line, or what its first message did.
    pub(crate) then: String,
}

pub(crate) fn created(created: &Created) -> String {
    let id = created.id;
    let mut out = vec![match &created.model {
        Some(model) => format!("created {id} on {model}"),
        None => format!("created {id}"),
    }];
    let place = match (&created.branch, created.project) {
        (Some(branch), _) => format!(" (its own worktree, on branch {branch})"),
        (None, Some(project)) => format!(" (a worktree of {project})"),
        (None, None) => String::new(),
    };
    out.push(format!("cwd: {}{place}", created.working));
    out.push(format!("gates: {}", created.gates));
    if created.watched {
        out.push("watched: wait() returns its statuses you have not seen".to_string());
    }
    out.extend(created.follow.clone());
    out.push(created.then.clone());
    out.join("\n")
}

/// The line giving an agent's status stream over `efference mcp` (docs/operating.md).
pub(crate) fn follow(url: &str) -> String {
    format!(
        "follow: {url} — in Claude Code, open a Monitor on it (ws) to be woken by each \
         decision point"
    )
}

/// The line in place of [`follow`] when the stream server could not be bound.
pub(crate) fn no_stream(error: &anyhow::Error) -> String {
    format!("follow: no stream ({error:#}); wait() reports its statuses")
}

/// Where a created session's gates go: to `decider`, unless it runs autonomous.
pub(crate) fn gates(decider: &str, autonomous: bool) -> String {
    match autonomous {
        true => "none: it runs autonomous, so a call its rungs do not clear is declined \
                 rather than held"
            .to_string(),
        false => format!("its gates come to {decider}"),
    }
}

/// A just-started session's status line: an idle session with no turn behind it says
/// it is waiting for one rather than that its turn is done.
pub(crate) fn fresh(view: &StatusView, reader: &Reader, now: u64) -> String {
    match &view.condition {
        Condition::Idle { closing: None } => {
            format!("{}: idle, waiting for its first message", view.id)
        }
        _ => first_line(view, reader, now),
    }
}

/// One line of an overview: the status's first line, with the sessions the reader
/// drives marked after their id.
pub(crate) fn overview_line(view: &StatusView, reader: &Reader, yours: bool, now: u64) -> String {
    let line = first_line(view, reader, now);
    match yours {
        true => match line.strip_prefix(&format!("{}:", view.id)) {
            Some(rest) => format!("{} (yours):{rest}", view.id),
            None => line,
        },
        false => line,
    }
}

/// A `wait` that ran `seconds` with nothing new: each watched agent's status line now
/// (docs/operating.md).
pub(crate) fn unchanged(seconds: u64, lines: Vec<String>) -> String {
    let mut out = vec![format!(
        "nothing new after {seconds}s; where each agent is now (not new):"
    )];
    out.extend(lines);
    out.join("\n")
}

/// The overview's lines, or what it says when there are none.
pub(crate) fn overview(lines: Vec<String>, empty: &str) -> String {
    match lines.is_empty() {
        true => empty.to_string(),
        false => lines.join("\n"),
    }
}

#[cfg(test)]
mod test;
