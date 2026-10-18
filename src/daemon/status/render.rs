//! The status as plain text (docs/tools.md): one renderer for every surface that says
//! where a session stands, given the view and who is reading it.
//!
//! The first line names the session, its condition, and what it needs. Every later
//! line opens with a label, and nothing rides on indentation, because a stream
//! consumer trims each line's leading whitespace. Only parts with something to say are
//! printed; instants become durations against the caller's clock; a cut says how much
//! it left out and which call reads the rest; senders are plain names.

use super::super::starting::Stage;
use super::moves::{Moves, Target};
use super::view::*;
use std::fmt::Write as _;

/// How the reader stands to the session (docs/tools.md).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Relation {
    /// The held call is routed to this reader to decide (docs/permissions.md).
    Reviewer,
    /// A must-wake subscriber: the session's creator or supervisor.
    Driver,
    /// A subscriber that is not woken, and does not drive.
    Observer,
}

/// Who reads a status, and which calls its mount makes the moves with.
pub(crate) struct Reader<'a> {
    pub(crate) relation: Relation,
    pub(crate) moves: &'a Moves,
    /// The reader's own session id: a sender or a decider by that name reads "you".
    pub(crate) me: Option<&'a str>,
    /// Whether the footer is printed. A read prints it; a notification leaves it out.
    pub(crate) footer: bool,
}

impl<'a> Reader<'a> {
    /// A subscription notification to the session `me`, which reads through the
    /// in-session tools.
    pub(crate) fn notification(relation: Relation, me: &'a str) -> Self {
        Self {
            relation,
            moves: &Moves::IN_SESSION,
            me: Some(me),
            footer: false,
        }
    }

    /// A read of the status — the footer printed — by the reader `me` (a session id or a
    /// client name), making its moves with `moves`.
    pub(crate) fn read(relation: Relation, moves: &'a Moves, me: Option<&'a str>) -> Self {
        Self {
            relation,
            moves,
            me,
            footer: true,
        }
    }

    /// Whether the reader drives the session, and so is told the moves that answer it.
    fn drives(&self) -> bool {
        self.relation != Relation::Observer
    }
}

/// `view` as `reader` reads it, with durations measured to `now` (Unix millis).
pub(crate) fn render(view: &StatusView, reader: &Reader, now: u64) -> String {
    let id = view.id.as_str();
    let moves = reader.moves;
    let mut target = Target {
        session: id,
        handle: None,
        seq: None,
    };
    let mut out: Vec<String> = Vec::new();
    match &view.condition {
        Condition::Starting {
            stage,
            authorizing,
            since,
        } => {
            let doing = match (stage, authorizing) {
                (_, Some(server)) => format!("waiting for {server} to be authorized"),
                (Stage::Environment, None) => "setting up its environment".to_string(),
                (Stage::Preparing, None) => "loading its config and tools".to_string(),
            };
            out.push(format!(
                "{id}: starting, {doing}, for {}",
                duration(now.saturating_sub(*since))
            ));
        }
        Condition::FailedToStart { reason } => out.push(format!("{id}: failed to start: {reason}")),
        Condition::Working { activity } => {
            let mut doing = activity.iter().map(|a| activity_text(a, now));
            match doing.next() {
                Some(first) => out.push(format!("{id}: working: {first}")),
                None => out.push(format!("{id}: working")),
            }
            out.extend(doing.map(|more| format!("also: {more}")));
        }
        Condition::Gated(held) => {
            target.handle = held.handle.as_deref();
            let mut call = held.tool.clone();
            if let Some(summary) = &held.summary {
                let _ = write!(call, " `{summary}`");
            }
            if let Some(handle) = &held.handle {
                let _ = write!(call, " (seq {handle})");
            }
            match reader.relation {
                Relation::Reviewer => {
                    out.push(format!("{id}: waiting for your decision on {call}"))
                }
                _ => {
                    let decider = match &held.reviewer {
                        Reviewer::Operator => "the operator".to_string(),
                        Reviewer::Supervisor { id: supervisor } => name(supervisor, reader.me),
                    };
                    out.push(format!(
                        "{id}: waiting for a decision from {decider} on {call}"
                    ));
                }
            }
            if let Some(intent) = &held.intent {
                out.push(format!("intent: {intent}"));
            }
            if let Some(reason) = &held.held_because {
                out.push(format!("held because: {reason}"));
            }
            if reader.relation == Relation::Reviewer {
                out.push(format!("decide: {}", (moves.decide)(&target)));
                if let Some(hand_back) = moves.hand_back {
                    out.push(format!("hand back: {}", hand_back(&target)));
                }
            }
        }
        Condition::Asking {
            question,
            n,
            of,
            handle,
        } => {
            target.handle = handle.as_deref();
            let mut place = Vec::new();
            if *of > 1 {
                place.push(format!("question {n} of {of}"));
            }
            if let Some(handle) = handle {
                place.push(format!("seq {handle}"));
            }
            let place = match place.is_empty() {
                true => String::new(),
                false => format!(" ({})", place.join(", ")),
            };
            out.push(format!("{id}: asking a question{place}"));
            out.push(format!("question: {}", question.trim_end()));
            if reader.drives() {
                out.push(format!("answer: {} answers it", (moves.answer)(&target)));
            }
        }
        Condition::Idle { closing } => {
            match closing {
                Some(closing) => {
                    target.seq = Some(closing.seq);
                    out.push(format!(
                        "{id}: idle, its turn is done (seq {})",
                        closing.seq
                    ));
                    said(&mut out, closing, reader, &target);
                }
                None => out.push(format!("{id}: idle, its turn is done")),
            }
            if reader.drives() {
                out.push(format!(
                    "next: {} sends its next message; {} ends it",
                    (moves.message)(&target),
                    (moves.archive)(&target)
                ));
            }
        }
        Condition::Stalled {
            message,
            held_triage,
        } => {
            out.push(format!("{id}: stopped: {message}"));
            if reader.drives() {
                out.push(match held_triage {
                    true => format!(
                        "next: {} retries it; a message waits until it clears",
                        (moves.resume)(&target)
                    ),
                    false => format!(
                        "next: {} retries it; {} sends a message instead",
                        (moves.resume)(&target),
                        (moves.message)(&target)
                    ),
                });
            }
        }
        Condition::Parked { park } => match park {
            Park::Expired { context_tokens } => {
                let cost = match context_tokens {
                    Some(tokens) => format!("{} tokens", tokens_text(*tokens)),
                    None => "its whole context".to_string(),
                };
                out.push(format!(
                    "{id}: paused before re-reading its context uncached ({cost})"
                ));
                if reader.drives() {
                    out.push(format!(
                        "next: {} goes ahead and pays that read, as any message does",
                        (moves.resume)(&target)
                    ));
                }
            }
            Park::Compacted => {
                out.push(format!(
                    "{id}: paused; its history was summarized while it waited"
                ));
                if reader.drives() {
                    out.push(format!(
                        "next: {} resumes it, as any message does",
                        (moves.resume)(&target)
                    ));
                }
            }
        },
        Condition::Blocked { block } => match block {
            Block::PaceHeld => out.push(format!(
                "{id}: held by its operator; nothing runs until the operator releases the hold"
            )),
            Block::ConfigRepair { path, parse_error } => {
                out.push(format!("{id}: blocked: its config file needs the operator"));
                if let Some(path) = path {
                    out.push(format!("file: {path} does not parse"));
                }
                // A read carries the parse error; a notification names the read.
                match parse_error {
                    Some(error) if reader.footer => out.push(format!("error: {error}")),
                    Some(_) if reader.drives() => out.push(format!(
                        "details: {} shows the parse error",
                        (moves.status)(&target)
                    )),
                    _ => {}
                }
            }
        },
        Condition::Ended => out.push(format!("{id}: ended; its log stays on disk")),
        Condition::Failed { error } => out.push(format!("{id}: ended with an error: {error}")),
    }
    if reader.footer {
        footer(&mut out, &view.footer, reader.me, now);
    }
    out.join("\n")
}

/// The closing message: whole for a driver up to the view's cap, its first line for an
/// observer, and a cut line naming what was left out and the read that has it.
fn said(out: &mut Vec<String>, closing: &Closing, reader: &Reader, target: &Target) {
    let rest = (reader.moves.transcript)(target);
    if reader.drives() {
        out.push(format!("said: {}", closing.text));
        let cut = closing.bytes.saturating_sub(closing.text.len());
        if cut > 0 {
            out.push(format!(
                "cut: {} more; {rest} shows the whole message",
                super::super::transcript::size(cut)
            ));
        }
        return;
    }
    let first = closing.text.lines().next().unwrap_or_default();
    out.push(format!("said: {first}"));
    let more = closing.lines.saturating_sub(1);
    if more > 0 {
        out.push(format!(
            "cut: {more} more {}; {rest} shows the whole message",
            plural(more, "line")
        ));
    } else if closing.bytes > first.len() {
        out.push(format!(
            "cut: {} more; {rest} shows the whole message",
            super::super::transcript::size(closing.bytes - first.len())
        ));
    }
}

fn footer(out: &mut Vec<String>, footer: &Footer, me: Option<&str>, now: u64) {
    let mut first = Vec::new();
    if let Some(model) = &footer.model {
        first.push(model.clone());
    }
    match (footer.context_used, footer.context_limit) {
        (Some(used), Some(limit)) => first.push(format!(
            "{} of {} context",
            tokens_text(used),
            tokens_text(limit)
        )),
        (Some(used), None) => first.push(format!("{} context", tokens_text(used))),
        _ => {}
    }
    if let Some(cost) = &footer.cost {
        let floor = if cost.unpriced > 0 { "at least " } else { "" };
        first.push(format!("{floor}${:.2} spent", cost.usd));
    }
    if !first.is_empty() {
        out.push(first.join(" · "));
    }
    if !footer.background.is_empty() {
        let processes: Vec<String> = footer
            .background
            .iter()
            .map(|p| {
                format!(
                    "p#{} `{}` for {}",
                    p.id,
                    p.command,
                    duration(now.saturating_sub(p.since))
                )
            })
            .collect();
        out.push(format!("background: {}", processes.join("; ")));
    }
    if !footer.queued.is_empty() {
        let mut senders: Vec<String> = Vec::new();
        for source in &footer.queued {
            let sender = sender(source.as_deref(), me);
            if !senders.contains(&sender) {
                senders.push(sender);
            }
        }
        let count = footer.queued.len();
        let held = match footer.held {
            true => ", held by its operator",
            false => "",
        };
        out.push(format!(
            "queued: {count} {}, from {}{held}",
            plural(count, "message"),
            list(&senders)
        ));
    }
}

pub(super) fn activity_text(activity: &Activity, now: u64) -> String {
    let since = |at: &u64| duration(now.saturating_sub(*at));
    match activity {
        Activity::Tool {
            tool,
            command: Some(command),
            since: at,
        } => format!("running {tool} `{command}` for {}", since(at)),
        Activity::Tool {
            tool, since: at, ..
        } => format!("running {tool} for {}", since(at)),
        Activity::Thinking { since: at } => format!("thinking for {}", since(at)),
        Activity::Triaging { since: at } => format!("filtering tool output for {}", since(at)),
        Activity::Compacting { since: at } => {
            format!("summarizing its history for {}", since(at))
        }
        Activity::Retrying {
            attempt,
            budget,
            next_at,
        } => {
            let mut text = match budget {
                Some(budget) => {
                    format!("retrying a failed request: attempt {attempt} of {budget} failed")
                }
                None => format!("retrying a failed request: attempt {attempt} failed"),
            };
            if let Some(next) = next_at {
                let _ = write!(text, ", next in {}", duration(next.saturating_sub(now)));
            }
            text
        }
        Activity::Paced { until } => format!(
            "waiting out its operator's pace delay, {} left",
            duration(until.saturating_sub(now))
        ),
    }
}

/// A turn's sender as a plain name: "operator" for the operator's own clients, "you"
/// for the reader, and otherwise the id or client name inside its `source` label
/// (docs/control-api.md).
fn sender(source: Option<&str>, me: Option<&str>) -> String {
    let Some(source) = source.filter(|source| *source != "web") else {
        return "operator".to_string();
    };
    let named = ["session[", "agent[", "mcp["]
        .iter()
        .find_map(|open| source.strip_prefix(open)?.strip_suffix(']'))
        .unwrap_or(source);
    name(named, me)
}

/// A session id, or "you" when it is the reader's.
pub(super) fn name(id: &str, me: Option<&str>) -> String {
    match me == Some(id) {
        true => "you".to_string(),
        false => id.to_string(),
    }
}

/// `a`, `a and b`, `a, b and c`.
fn list(items: &[String]) -> String {
    match items {
        [] => String::new(),
        [one] => one.clone(),
        [init @ .., last] => format!("{} and {last}", init.join(", ")),
    }
}

fn plural(n: usize, noun: &str) -> String {
    match n {
        1 => noun.to_string(),
        _ => format!("{noun}s"),
    }
}

/// `12s`, `3m`, `5h`, `2d`: a span in its largest whole unit.
pub(crate) fn duration(millis: u64) -> String {
    let seconds = millis / 1000;
    match seconds {
        s if s < 60 => format!("{s}s"),
        s if s < 60 * 60 => format!("{}m", s / 60),
        s if s < 48 * 60 * 60 => format!("{}h", s / 3600),
        s => format!("{}d", s / 86_400),
    }
}

/// `950`, `48k`, `1M`, `1.2M`: a token count at the precision a reader weighs it by.
fn tokens_text(n: u64) -> String {
    match n {
        n if n >= 999_500 => {
            let millions = format!("{:.1}", n as f64 / 1_000_000.0);
            format!("{}M", millions.trim_end_matches(".0"))
        }
        n if n >= 1000 => format!("{}k", (n + 500) / 1000),
        n => n.to_string(),
    }
}

#[cfg(test)]
mod test;
