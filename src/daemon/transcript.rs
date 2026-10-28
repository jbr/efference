//! The transcript: a log window as plain text, at the grain a reader asks for
//! (docs/control-api.md).
//!
//! `GET /transcript` has two grains and a zoom:
//!
//! - **conversation** (`conversation.rs`, the default): what was said and done. Prose
//!   in full, each call on one line with its outcome — who decided its gate, its
//!   result's size, and what the session saw of it — and none of the mechanism.
//! - **audit** (`audit.rs`): one block per log entry, the mechanism included.
//! - **zoom** (`zoom.rs`, `seq=8.2`): one entry in full — a call's exact input, its
//!   decision trail, and its raw result, capped.
//!
//! A result's body is never part of a window; the zoom and `GET /log/:seq` are where
//! it is read. The TUI keeps its own tree (docs/tui.md), which shows the bodies.

use crate::log::{
    AssistantOutput, CallId, ConfigChange, ContentBlock, EditState, LogEntry, MessagesOutputBlock,
    Payload, ProcessEventKind, RawResult, ResponsesMessageContent, ResponsesOutputItem, Stance,
    ToolCall, WireState,
};
use crate::project::selection;
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::fmt::Write as _;

pub(crate) mod audit;
pub(crate) mod conversation;
pub(crate) mod cursor;
#[cfg(test)]
mod fixtures;
mod index;
pub(crate) mod reference;
pub(crate) mod zoom;

pub(crate) use zoom::Handle;

/// Which render a window read answers with (docs/control-api.md).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Grain {
    /// What was said and done, each call with its outcome (`conversation.rs`).
    #[default]
    Conversation,
    /// One block per log entry, the mechanism included (`audit.rs`).
    Audit,
}

/// Who reads a conversation or a zoom, and the one piece of live state it shows.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Reading<'a> {
    /// The session read, by id: the header names it, and its own prose is labelled
    /// with it. `None` where no id is known (tests, offline reads).
    pub(crate) session: Option<&'a str>,
    /// The reader, as a `source` (`session[<id>]`, `mcp[<name>]`) or a session id:
    /// a sender or decider by that name reads "you" ([`sender`]).
    pub(crate) me: Option<&'a str>,
    /// The call held at the gate now, which reads as waiting for a decision.
    pub(crate) held: Option<&'a str>,
}

impl Reading<'_> {
    /// `transcript of <id>`, or `transcript` with no id.
    fn title(&self) -> String {
        match self.session {
            Some(id) => format!("transcript of {id}"),
            None => "transcript".to_string(),
        }
    }

    /// Whether the reader can fetch a body by route (`GET /log/:seq`): the operator's
    /// command line and a plain HTTP client name no reader, where a named reader (an
    /// in-session agent, an MCP driver) has only the tools.
    fn reads_by_route(&self) -> bool {
        self.me.is_none()
    }

    /// The label on the session's own prose: its id, or `assistant` with none.
    fn speaker(&self) -> &str {
        self.session.unwrap_or("assistant")
    }
}

/// A sender or a decider as a plain name for the reader `me` (docs/control-api.md):
/// `operator` for no source, `operator (web)` for the web client, the id or client
/// name inside a `session[…]`, `agent[…]` or `mcp[…]` label (a session's without the
/// project it may name), `<id> (hand-off)` for a
/// hand-off brief, `the permission review` for the loop's own (`review[<level>]`), any
/// other label as written, and `you` where `me` names the label or the name inside it.
pub(crate) fn sender(source: Option<&str>, me: Option<&str>) -> String {
    let Some(source) = source else {
        return "operator".to_string();
    };
    if source == "web" {
        return "operator (web)".to_string();
    }
    if crate::frontend::is_review_source(source) {
        return "the permission review".to_string();
    }
    let named = |name: &str| match me.is_some_and(|me| me == source || me == name) {
        true => "you".to_string(),
        false => name.to_string(),
    };
    if let Some(predecessor) = crate::frontend::handoff_predecessor(source) {
        return format!("{} (hand-off)", named(predecessor));
    }
    named(crate::frontend::source_name(source))
}

/// `ok, 12 lines` — a result's outcome and size: `ok` or `error`, its text in lines,
/// and its images; `no output` when it carried nothing.
fn outcome(raw: &RawResult) -> String {
    let mut parts = vec![if raw.is_error { "error" } else { "ok" }.to_string()];
    match raw.content.lines().count() {
        0 if raw.images.is_empty() => parts.push("no output".to_string()),
        0 => {}
        n => parts.push(count(n, "line")),
    }
    if !raw.images.is_empty() {
        parts.push(count(raw.images.len(), "image"));
    }
    parts.join(", ")
}

/// `it saw 1: note` — what the session's view kept of a result, given its final
/// wire (`None` while no verdict has decided it). `of_total` states the result's
/// length beside a partial count, for a line that does not already say it.
fn saw(raw: &RawResult, wire: Option<&WireState>, of_total: bool) -> String {
    match wire {
        None | Some(WireState::Pending) => "it has not seen it yet".to_string(),
        Some(WireState::Raw) => "it saw all of it".to_string(),
        Some(WireState::Elided) => "it saw none of it".to_string(),
        Some(WireState::Interrupted(interruption)) => format!(
            "it saw none of it, but {}",
            crate::project::render_interruption(interruption)
        ),
        Some(WireState::TriageFailed { why }) => format!("it saw none of it ({})", flat(why)),
        Some(WireState::Triaged(verdict)) => {
            let (kept, total) = kept_of(raw, verdict);
            let mut text = if kept >= total {
                "it saw all of it".to_string()
            } else if kept == 0 {
                "it saw none of it".to_string()
            } else if of_total {
                format!("it saw {kept} of {}", count(total, "line"))
            } else {
                format!("it saw {kept}")
            };
            if let Some(note) = &verdict.note {
                let _ = write!(text, ": {}", flat(note));
            }
            text
        }
    }
}

/// How many lines a verdict kept, of how many the result has (the arithmetic the
/// entry view's `selection` carries, `entry_view.rs`).
fn kept_of(raw: &RawResult, verdict: &crate::log::Triaged) -> (usize, usize) {
    let d = selection(&raw.content, &verdict.keep);
    let kept = d.ranges.iter().map(|&(a, b)| b - a + 1).sum();
    (kept, d.shown.len() + d.dropped)
}

/// `allowed by parent` — a gate's recorded decision, its decider named for `me`. An
/// allow carries the decider's note when one was sent with it (`note`, from
/// `index.rs`): `allowed by you, with "use read next time"`.
fn decided(decision: &crate::log::GateDecision, me: Option<&str>, note: Option<&str>) -> String {
    use crate::frontend::Approval;
    let by = sender(decision.source.as_deref(), me);
    match &decision.decision {
        Approval::Approve => match note {
            Some(note) => format!("allowed by {by}, with \"{}\"", flat(note)),
            None => format!("allowed by {by}"),
        },
        Approval::Abort => format!("denied by {by}"),
        Approval::Redirect(message) if message.trim().is_empty() => format!("denied by {by}"),
        Approval::Redirect(message) => format!("denied by {by}: {}", flat(message)),
        Approval::ConfigEdit { change, .. } => {
            format!("granted by {by}: {}", change_words(change))
        }
    }
}

/// `escalated by p to g: "<note>"` — one escalation of a held call, its escalator and
/// the session it went to named for `me` (docs/permissions.md).
fn escalated(escalation: &crate::log::Escalation, me: Option<&str>) -> String {
    let by = sender(escalation.source.as_deref(), me);
    let to = match escalation.to.as_deref() {
        Some(to) => sender(Some(to), me),
        None => "the operator".to_string(),
    };
    match escalation.note.as_deref().map(flat) {
        Some(note) if !note.is_empty() => format!("escalated by {by} to {to}: \"{note}\""),
        _ => format!("escalated by {by} to {to}"),
    }
}

/// Whether a recorded decision refused the call: an abort or a redirect.
fn refused(decision: &crate::log::GateDecision) -> bool {
    use crate::frontend::Approval;
    matches!(decision.decision, Approval::Abort | Approval::Redirect(_))
}

/// A config change in plain words, such as command `grep -n` or write access to `src`.
fn change_words(change: &ConfigChange) -> String {
    match change {
        ConfigChange::Command { command } => format!("command {}", code_span(command)),
        ConfigChange::Path {
            path, write: true, ..
        } => format!("write access to {}", code_span(path)),
        ConfigChange::Path {
            path, read: true, ..
        } => format!("read access to {}", code_span(path)),
        ConfigChange::Path { path, .. } => format!("no access to {}", code_span(path)),
        ConfigChange::Tool { name } => {
            format!("the tool {}", code_span(&crate::mcp::display_name(name)))
        }
        ConfigChange::Scope { .. } => "a new scope".to_string(),
    }
}

/// `text` as one Markdown code span: fenced by a backtick run one longer than the
/// longest run `text` contains, with a space inside each fence when `text` opens or
/// closes with a backtick (the CommonMark code-span rule). Text with no backtick is
/// fenced by a single one.
pub(crate) fn code_span(text: &str) -> String {
    let longest = text
        .split(|c| c != '`')
        .map(str::len)
        .max()
        .unwrap_or_default();
    let fence = "`".repeat(longest + 1);
    match text.starts_with('`') || text.ends_with('`') {
        true => format!("{fence} {text} {fence}"),
        false => format!("{fence}{text}{fence}"),
    }
}

/// A user entry's text blocks, joined; its attachments are lines of their own.
fn text_of(blocks: &[ContentBlock]) -> String {
    blocks
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text(text) => Some(text.trim_end()),
            _ => None,
        })
        .filter(|text| !text.is_empty())
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// `1 line`, `12 lines`.
fn count(n: usize, noun: &str) -> String {
    match n {
        1 => format!("1 {noun}"),
        n => format!("{n} {noun}s"),
    }
}

/// Text kept to one line, its newlines marked as [`clip`] marks them.
fn flat(text: &str) -> String {
    text.trim().replace('\n', "⏎")
}

/// A rendered tool-call argument value is cut here, with a marker: a `write`'s
/// `contents` is a body, and bodies are what this view exists to leave out.
const ARG_CAP: usize = 120;
/// The cut for every other argument. A `command` is what a driver reads to decide a
/// gate and a `pattern` or `path` is how it tells two calls apart, so those get room;
/// only the keys in [`BODY_KEYS`] are cut at [`ARG_CAP`].
const ARG_WIDE: usize = 400;
/// Argument keys whose value is a body rather than an address or a command. A
/// heuristic by name, like the verbatim-handle shapes in `triage/handles.rs`: a tool
/// that names its body something else gets the wide cut, which is a longer line, not
/// a wrong one.
const BODY_KEYS: &[&str] = &[
    "contents",
    "content",
    "text",
    "body",
    "new_string",
    "old_string",
];

/// A call's handle in the transcript: the bare `seq` of the assistant entry that
/// issued it when it was alone there, `seq.n` (1-based) when it shared the entry.
/// The status names a held call by the same handle (`status/view.rs`).
pub(crate) fn call_label(seq: crate::log::Seq, index: usize, calls: usize) -> String {
    if calls > 1 {
        format!("{seq}.{}", index + 1)
    } else {
        seq.to_string()
    }
}

/// The handle of call `call_id`, read off the latest assistant entry that issued it;
/// `None` when no entry in `entries` did.
pub(crate) fn call_handle(entries: &[LogEntry], call_id: &str) -> Option<String> {
    entries.iter().rev().find_map(|entry| {
        let Payload::Assistant { output, .. } = &entry.payload else {
            return None;
        };
        let (_, calls) = assistant(output);
        let index = calls.iter().position(|call| call.id.0 == call_id)?;
        Some(call_label(entry.seq, index, calls.len()))
    })
}

/// An assistant turn's text blocks and its calls, in provider order within each.
fn assistant(output: &AssistantOutput) -> (Vec<String>, Vec<ToolCall>) {
    let mut prose = Vec::new();
    let mut calls = Vec::new();
    match output {
        AssistantOutput::Messages(blocks) => {
            for block in blocks {
                match block {
                    MessagesOutputBlock::Text(text) if !text.trim().is_empty() => {
                        prose.push(text.trim().to_string());
                    }
                    MessagesOutputBlock::ToolUse(call) => calls.push(call.clone()),
                    _ => {}
                }
            }
        }
        AssistantOutput::Responses(items) => {
            for item in items {
                match item {
                    ResponsesOutputItem::Message { content, .. } => {
                        for part in content {
                            let text = match part {
                                ResponsesMessageContent::OutputText { text, .. } => text,
                                ResponsesMessageContent::Refusal { refusal } => refusal,
                            };
                            if !text.trim().is_empty() {
                                prose.push(text.trim().to_string());
                            }
                        }
                    }
                    ResponsesOutputItem::FunctionCall {
                        call_id,
                        name,
                        arguments,
                        ..
                    } => {
                        let input = serde_json::from_str(arguments)
                            .unwrap_or_else(|_| Value::String(arguments.clone()));
                        calls.push(ToolCall::new(name, call_id.clone(), input));
                    }
                    _ => {}
                }
            }
        }
        AssistantOutput::Completions(message) => {
            prose.extend(
                message
                    .text()
                    .into_iter()
                    .map(str::trim)
                    .filter(|text| !text.is_empty())
                    .map(str::to_string),
            );
            // A call whose arguments do not parse shows no calls rather than a
            // partial list; such a turn is record-only and never dispatched.
            calls.extend(message.tool_calls().unwrap_or_default());
        }
    }
    (prose, calls)
}

/// What a held call does, in one line for its status (`status/view.rs`): a `bash`
/// call's command, any other call's arguments as [`call_line`] writes them. `None`
/// for a call with nothing but its intent.
pub(crate) fn call_summary(call: &ToolCall) -> Option<String> {
    if call.name == crate::bash::NAME
        && let Some(command) = call.input.get("command").and_then(Value::as_str)
    {
        return Some(clip(command, ARG_WIDE));
    }
    let arguments = arguments(call);
    let arguments = arguments.trim_start();
    (!arguments.is_empty()).then(|| arguments.to_string())
}

/// ` key=value key=value` — every argument but the intent, each with a leading
/// space, cut as [`call_line`] describes.
fn arguments(call: &ToolCall) -> String {
    let mut line = String::new();
    match &call.input {
        Value::Object(map) => {
            for (key, value) in map {
                if key == crate::tools::INTENT {
                    continue;
                }
                let rendered = match value {
                    Value::String(s) => s.clone(),
                    other => other.to_string(),
                };
                let cap = if BODY_KEYS.contains(&key.as_str()) {
                    ARG_CAP
                } else {
                    ARG_WIDE
                };
                let _ = write!(line, " {key}={:?}", clip(&rendered, cap));
            }
        }
        other => {
            let _ = write!(line, " {}", clip(&other.to_string(), ARG_WIDE));
        }
    }
    line
}

fn clip(s: &str, cap: usize) -> String {
    let flat = s.replace('\n', "⏎");
    match flat.char_indices().nth(cap) {
        Some((i, _)) => format!("{}…", &flat[..i]),
        None => flat,
    }
}

/// `512 B`, `2.1 KB`, `1.3 MB`: a body's size as the transcript and the status
/// state it.
pub(crate) fn size(bytes: usize) -> String {
    if bytes < 1024 {
        format!("{bytes} B")
    } else if bytes < 1024 * 1024 {
        format!("{:.1} KB", bytes as f64 / 1024.0)
    } else {
        format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
    }
}

#[cfg(test)]
mod test;
