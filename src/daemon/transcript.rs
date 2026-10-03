//! The transcript: a log window at the grain a driver reads (docs/control-api.md).
//!
//! `GET /transcript` renders each entry as one text block: prose in full, a tool call
//! as its name, arguments and `intent`, a result as its size, its error flag and its
//! triage verdict — never its body. The bodies stay behind `GET /log/:seq`, which is
//! where a reader goes after this view names the seq. Entries a driver never acts on
//! render to nothing: telemetry, and a triage that pinned or dismissed nothing.
//!
//! One render for the control channel and the command line; the TUI keeps its own
//! tree (docs/tui.md), which shows the bodies this view leaves out.

use crate::log::{
    AssistantOutput, CallId, ConfigChange, EditState, LogEntry, MessagesOutputBlock, Payload,
    ProcessEventKind, RawResult, ResponsesMessageContent, ResponsesOutputItem, Stance, ToolCall,
    WireState,
};
use crate::project::distillate;
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::fmt::Write as _;

/// Widest tag, which sets the column the bodies start in.
const TAG_WIDTH: usize = 9;
/// The label column: room for a five-digit seq with a `.n` call suffix. A wider
/// label shifts its own line, nothing else.
const LABEL_WIDTH: usize = 7;
/// A rendered tool-call argument value is cut here, with a marker: a `write`'s
/// `contents` is a body, and bodies are what this view exists to leave out.
const ARG_CAP: usize = 120;
/// The cut for every other argument. A `command` is what a driver reads to decide a
/// gate and a `pattern` or `path` is how it tells two calls apart, so those get room;
/// only the keys in [`BODY_KEYS`] are cut at [`ARG_CAP`].
const ARG_WIDE: usize = 400;
/// Argument keys whose value is a body rather than an address or a command. A
/// heuristic by name, like the verbatim-handle shapes in `distill/handles.rs`: a tool
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

/// Render `entries` in order, one block per entry that has something to say.
pub(crate) fn render(entries: &[LogEntry]) -> String {
    let mut out = String::new();
    // The title peek appends an entry even when it keeps the text (docs/session-title.md);
    // a reader wants to see the title when it moves, not after every turn.
    let mut title: Option<&str> = None;
    // Calls that shared an entry with others, by id, labelled `seq.n`. A result's
    // line names its call's label so the pairing survives the interleaving; a call
    // that was alone in its entry needs no label — the entry above it is the call.
    let mut siblings: HashMap<CallId, String> = HashMap::new();
    // Calls that were `ask`s: their results are the operator's reply, not a tool's.
    let mut asks: HashSet<CallId> = HashSet::new();
    for entry in entries {
        if let Payload::Title(t) = &entry.payload {
            if title == Some(t.text.as_str()) {
                continue;
            }
            title = Some(&t.text);
        }
        for Line { label, tag, body } in lines(entry, &mut siblings, &mut asks) {
            let label = label.unwrap_or_else(|| entry.seq.to_string());
            block(&mut out, &label, tag, &body);
        }
    }
    out
}

/// One rendered line: the entry's seq is the default label; a call among several in
/// one entry carries `seq.n` instead.
struct Line {
    label: Option<String>,
    tag: &'static str,
    body: String,
}

fn line(tag: &'static str, body: impl Into<String>) -> Line {
    Line {
        label: None,
        tag,
        body: body.into(),
    }
}

/// `label tag body`, with every line of the body after the first indented to the
/// body column so a multi-line message reads as one block.
fn block(out: &mut String, label: &str, tag: &str, body: &str) {
    let mut lines = body.lines();
    let first = lines.next().unwrap_or("");
    let _ = writeln!(out, "{label:>LABEL_WIDTH$} {tag:<TAG_WIDTH$} {first}");
    for line in lines {
        let _ = writeln!(out, "{:>LABEL_WIDTH$} {:<TAG_WIDTH$} {line}", "", "");
    }
}

/// The lines one entry renders as. Most entries are one; an assistant turn is one
/// per text block and per call; some are none.
fn lines(
    entry: &LogEntry,
    siblings: &mut HashMap<CallId, String>,
    asks: &mut HashSet<CallId>,
) -> Vec<Line> {
    match &entry.payload {
        Payload::User(blocks) => {
            // A sourced turn opens its line with the same `source:` address it
            // carries on the wire (docs/control-api.md), so a reader attributes
            // the prose the way the model does; an operator turn has none.
            let mut source = entry.source.as_deref();
            blocks
                .iter()
                .filter_map(|b| match b {
                    crate::log::ContentBlock::Text(t) => {
                        let body = match source.take() {
                            Some(source) => format!("{source}: {t}"),
                            None => t.clone(),
                        };
                        Some(line("user", body))
                    }
                    crate::log::ContentBlock::Attachment(arrival) => {
                        Some(line("user", format!("attached {}", arrival.path)))
                    }
                    _ => None,
                })
                .collect()
        }
        Payload::Assistant {
            output,
            upstream_failure,
        } => {
            let mut v = Vec::new();
            if let Some(failure) = upstream_failure {
                v.push(line(
                    "assistant",
                    format!("(record-only, never projected: {})", failure.why()),
                ));
            }
            let (prose, calls) = assistant(output);
            v.extend(prose.into_iter().map(|text| line("assistant", text)));
            let several = calls.len() > 1;
            for (i, call) in calls.iter().enumerate() {
                // A question put to the operator reads as a turn ending: its text, not
                // the call (docs/control-api.md).
                let mut l = match crate::supervisor::ask_of_call(&calls, i) {
                    Some((question, n, of)) => {
                        asks.insert(call.id.clone());
                        let place = if of > 1 {
                            format!(" (question {n} of {of})")
                        } else {
                            String::new()
                        };
                        line("asked", format!("{question}{place}"))
                    }
                    None => line("call", call_line(call)),
                };
                if several {
                    let label = format!("{}.{}", entry.seq, i + 1);
                    siblings.insert(call.id.clone(), label.clone());
                    l.label = Some(label);
                }
                v.push(l);
            }
            v
        }
        // An ask's result is the operator's reply when a submission produced it, and
        // reads like the `User` line a sourced turn gets; the error results (an
        // interrupt, a malformed call) carry no submission and say so.
        Payload::ToolResult { call_id, raw, .. } if asks.contains(call_id) => {
            if entry.submission_id.is_none() {
                return vec![line("result", "not answered")];
            }
            let body = match entry.source.as_deref() {
                Some(source) => format!("{source}: {}", raw.content),
                None => raw.content.clone(),
            };
            vec![line("user", body)]
        }
        Payload::ToolResult { call_id, raw, wire } => {
            let mut body = result(raw, wire);
            if let Some(label) = siblings.get(call_id) {
                body = format!("for {label}: {body}");
            }
            vec![line("result", body)]
        }
        Payload::Mark(mark) => {
            let crate::log::MarkKind::Boundary { upto } = &mark.kind;
            vec![line(
                "mark",
                format!("boundary through seq {upto}: {}", mark.note),
            )]
        }
        Payload::Triage(triage) => {
            let mut parts = Vec::new();
            for (seq, disposition) in &triage.inbox {
                let verb = match disposition {
                    crate::log::Disposition::Pin => "pinned",
                    crate::log::Disposition::Dismiss => "dismissed",
                };
                parts.push(format!("{verb} notification #{seq}"));
            }
            for retry in &triage.retries {
                parts.push(format!("retry: {retry}"));
            }
            if parts.is_empty() {
                vec![]
            } else {
                vec![line("triage", parts.join("; "))]
            }
        }
        Payload::Ambient(n) => vec![line("notify", format!("[{}] {}", n.source, n.body))],
        Payload::Spot(spot) => {
            let stance = match spot.stance {
                Stance::Clear => "clear",
                Stance::ClearAlways => "clear_always",
                Stance::Watch => "watch",
                Stance::Gate => "gate",
            };
            let mut body = format!("{stance}: {}", spot.reason);
            if let Some(prefix) = &spot.probed_prefix {
                let _ = write!(body, " (probed `{prefix}`)");
            }
            vec![line("spot", body)]
        }
        Payload::ToolTelemetry(_) | Payload::CallStarted { .. } => vec![],
        Payload::Resumed(resumed) => vec![line(
            "resumed",
            crate::project::render_resumed(resumed, entry.seq),
        )],
        Payload::ProcessEvent(event) => {
            let body = match &event.kind {
                ProcessEventKind::Spawned(spawn) => {
                    format!("p#{} spawned: {}", event.id, spawn.command)
                }
                ProcessEventKind::Exited(exit) => format!(
                    "p#{} exited {:?} after {} ms",
                    event.id, exit.outcome, exit.duration_ms
                ),
            };
            vec![line("process", body)]
        }
        Payload::ConfigEdit(edit) => {
            let state = match edit.state {
                EditState::Proposed => "proposed",
                EditState::Committed => "committed",
                EditState::Cancelled => "cancelled",
            };
            let change = match &edit.change {
                ConfigChange::Command { command } => format!("command {command:?}"),
                ConfigChange::Path { path, read, write } => {
                    format!("path {path:?} read={read} write={write}")
                }
                ConfigChange::Tool { name } => format!("tool {name:?}"),
                ConfigChange::Scope { text } => format!("scope {text:?}"),
            };
            let mut body = format!("{state} {change}");
            if let Some(seq) = edit.resolves {
                let _ = write!(body, " (resolves #{seq})");
            }
            if !edit.note.is_empty() {
                let _ = write!(body, ": {}", edit.note);
            }
            vec![line("config", body)]
        }
        Payload::Preamble { system, .. } => {
            vec![line(
                "preamble",
                format!("{} of system prompt", size(system.len())),
            )]
        }
        Payload::Title(title) => vec![line("title", title.text.clone())],
        Payload::ResultVerdict { for_result, .. } => vec![line(
            "result_verdict",
            format!("triage resolved result #{for_result}"),
        )],
    }
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
    }
    (prose, calls)
}

/// `name key=value key=value  intent: …` — every argument but the intent, a body
/// key's value cut at [`ARG_CAP`] and any other at [`ARG_WIDE`]; the intent last,
/// in full, since it is the one line the model wrote for a reader.
fn call_line(call: &ToolCall) -> String {
    let mut line = call.name.clone();
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
    if let Some(intent) = call.intent() {
        let _ = write!(line, "  intent: {intent}");
    }
    line
}

/// `ok 2.1 KB, kept 32/82 lines: note` — the result's outcome, size and what the
/// model's view kept of it, plus where its images are.
fn result(raw: &RawResult, wire: &WireState) -> String {
    let mut body = String::from(if raw.is_error { "ERROR" } else { "ok" });
    let total = raw
        .content_elided
        .as_ref()
        .map_or(raw.content.len(), |e| e.total_bytes);
    let _ = write!(body, " {}", size(total));
    match wire {
        WireState::Raw => body.push_str(", raw"),
        WireState::Pending => body.push_str(", awaiting triage"),
        WireState::Elided => body.push_str(", elided"),
        WireState::TriageFailed { why } => {
            let _ = write!(body, ", triage failed: {why}");
        }
        WireState::Distilled(verdict) => {
            let d = distillate(&raw.content, &verdict.keep);
            let kept: usize = d.ranges.iter().map(|&(a, b)| b - a + 1).sum();
            let _ = write!(body, ", kept {kept}/{} lines", d.shown.len() + d.dropped);
            if let Some(note) = &verdict.note {
                let _ = write!(body, ": {note}");
            }
            if let Some(feedback) = &verdict.intent_feedback {
                let _ = write!(body, " (intent feedback: {feedback})");
            }
        }
    }
    if !raw.images.is_empty() {
        let _ = write!(
            body,
            "; {} image(s), fetch with `image` at this seq",
            raw.images.len()
        );
    }
    body
}

fn clip(s: &str, cap: usize) -> String {
    let flat = s.replace('\n', "⏎");
    match flat.char_indices().nth(cap) {
        Some((i, _)) => format!("{}…", &flat[..i]),
        None => flat,
    }
}

fn size(bytes: usize) -> String {
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
