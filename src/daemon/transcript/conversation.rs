//! The conversation grain (`GET /transcript`, docs/control-api.md): what a session
//! said and did, each call and its outcome on one line, the mechanism left out.
//!
//! What became of a call is looked up across the whole log (`index.rs`), not the
//! window, so a cut anywhere neither hides an outcome that landed after it nor leaves
//! a result without its call's handle. No line's meaning rides on indentation, since a
//! stream consumer trims leading whitespace.

use super::index::Index;
use super::*;
use crate::record::log::{ContentBlock, Exclusion, GateDecision, Seq, Telemetry};
use std::ops::RangeInclusive;

mod tail;

/// `window`, a contiguous slice of `log`, as `reading` reads it.
pub(crate) fn render(log: &[LogEntry], window: &[LogEntry], reading: &Reading) -> String {
    finish(Out::run(log, window, reading), window, 0)
}

/// The last `tail` conversation lines of `range`, a contiguous slice of `log`, as
/// `reading` reads them (`tail.rs`).
pub(crate) fn render_tail(
    log: &[LogEntry],
    range: &[LogEntry],
    tail: u64,
    reading: &Reading,
) -> String {
    if tail == 0 {
        return render(log, &[], reading);
    }
    let Some(cut) = tail::cut(log, range, tail, reading) else {
        return render(log, range, reading);
    };
    let window = &range[cut.start..];
    finish(Out::run(log, window, reading), window, cut.earlier)
}

/// The header and the lines of a window run through [`Out::run`]. `earlier` is how many
/// lines come before the window in its range, when a tail cut it short.
fn finish(out: Out, window: &[LogEntry], earlier: usize) -> String {
    let title = out.reading.title();
    let (Some(first), Some(last)) = (window.first(), window.last()) else {
        return format!("{title} (no entries)\n");
    };
    let mut header = match first.seq == last.seq {
        true => format!("{title}, seq {}", first.seq),
        false => format!("{title}, seq {}–{}", first.seq, last.seq),
    };
    if earlier > 0 {
        let _ = write!(
            header,
            ": the last {}; {} before — since: 0 reads from the start",
            count(out.lines.len(), "line"),
            count(earlier, "line")
        );
    }
    let mut lines = vec![header];
    lines.extend(out.lines.into_iter().map(|(_, line)| line));
    if lines.len() == 1 {
        lines.push("(nothing said or done in this range)".to_string());
    }
    let mut text = lines.join("\n");
    text.push('\n');
    text
}

struct Out<'a, 'r> {
    /// The rendered lines, each with the seq of the entry that produced it.
    lines: Vec<(Seq, String)>,
    index: Index<'a>,
    /// The seqs the window covers.
    window: RangeInclusive<Seq>,
    reading: &'r Reading<'r>,
    /// The entry being rendered.
    at: Seq,
}

impl<'a, 'r> Out<'a, 'r> {
    /// Every entry of `window`, rendered. An empty window renders nothing.
    fn run(log: &'a [LogEntry], window: &'a [LogEntry], reading: &'r Reading<'r>) -> Self {
        let (first, last) = match (window.first(), window.last()) {
            (Some(first), Some(last)) => (first.seq, last.seq),
            _ => (0, 0),
        };
        let mut out = Self {
            lines: Vec::new(),
            index: Index::new(log),
            window: first..=last,
            reading,
            at: first,
        };
        for entry in window {
            out.entry(entry);
        }
        out
    }

    fn push(&mut self, label: impl std::fmt::Display, body: impl std::fmt::Display) {
        self.lines.push((self.at, format!("{label}  {body}")));
    }

    /// Whether the call `id` is issued inside the window, where its line carries
    /// everything that became of it.
    fn shown(&self, id: &str) -> bool {
        self.index
            .call(id)
            .is_some_and(|call| self.window.contains(&call.seq))
    }

    fn name(&self, source: Option<&str>) -> String {
        sender(source, self.reading.me)
    }

    fn entry(&mut self, entry: &'a LogEntry) {
        self.at = entry.seq;
        match &entry.payload {
            Payload::User(blocks) => self.user(entry, blocks),
            Payload::Assistant { output, excluded } => match excluded {
                // Record-only: the session never read it, and its calls never ran.
                Some(Exclusion::Upstream(failure)) => self.push(
                    entry.seq,
                    format!("a reply did not complete: {}", failure.why()),
                ),
                Some(Exclusion::Empty) => self.push(
                    entry.seq,
                    "a reply came back with neither prose nor a call, and was asked again"
                        .to_string(),
                ),
                None => self.assistant(entry.seq, output),
            },
            Payload::ToolResult { call_id, .. } if !self.shown(&call_id.0) => {
                self.orphan_result(entry, &call_id.0);
            }
            Payload::ResultVerdict { for_result, wire } => self.orphan_verdict(*for_result, wire),
            Payload::Telemetry(Telemetry::GateDecision(decision))
                if !self.shown(&decision.call_id.0) =>
            {
                let label = self.label(&decision.call_id.0, entry.seq);
                let body = self.decision_words(decision);
                self.push(label, body);
            }
            Payload::Telemetry(Telemetry::Escalation(escalation))
                if !self.shown(&escalation.call_id.0) =>
            {
                let label = self.label(&escalation.call_id.0, entry.seq);
                let body = escalated(escalation, self.reading.me);
                self.push(label, body);
            }
            Payload::Ambient(note) => {
                let from = self.name(Some(&note.source));
                self.push(
                    entry.seq,
                    format!("note from {from}: {}", note.body.trim_end()),
                );
            }
            Payload::Summary(summary) => self.push(
                entry.seq,
                format!("summarized seq {}–{}", summary.from, summary.upto),
            ),
            Payload::Telemetry(Telemetry::Continue(origin)) => {
                let from = self.name(origin.source.as_deref());
                self.push(entry.seq, format!("{from} continued it"));
            }
            Payload::Archived { .. } => self.push(entry.seq, "archived"),
            Payload::Resumed(_) => self.push(entry.seq, "resumed after its host restarted"),
            // On its call's line (a result, a decision, an escalation whose call is in
            // the window), or the mechanism.
            _ => {}
        }
    }

    /// The recorded decision in words, with the note an allow was sent with.
    fn decision_words(&self, decision: &GateDecision) -> String {
        let note = self.index.note(&decision.call_id.0);
        decided(decision, self.reading.me, note.as_deref())
    }

    /// A call's handle, or `seq` when no assistant entry issued the call.
    fn label(&self, call_id: &str, seq: Seq) -> String {
        self.index
            .call(call_id)
            .map_or_else(|| seq.to_string(), |call| call.handle.clone())
    }

    fn user(&mut self, entry: &LogEntry, blocks: &[ContentBlock]) {
        // The kickoff belongs to the session's opening, as the preamble does
        // (docs/context.md).
        if entry.source.as_deref() == Some(crate::record::names::sources::KICKOFF_SOURCE) {
            return;
        }
        // An allow's note is on its call's line, or on the decision's.
        if let Some(call_id) = self.index.note_for(entry.seq).map(str::to_string) {
            let decision_shown = self
                .index
                .decision(&call_id)
                .is_some_and(|(seq, _)| self.window.contains(&seq));
            if self.shown(&call_id) || decision_shown {
                return;
            }
        }
        // A redirect's message is on its call's line, or on the decision's.
        if let Some(call_id) = self.index.steer(entry.seq).map(str::to_string) {
            let decision = self.index.decision(&call_id);
            let decision_shown = decision.is_some_and(|(seq, _)| self.window.contains(&seq));
            if self.shown(&call_id) || decision_shown {
                return;
            }
            let by = decision.and_then(|(_, d)| d.source.as_deref());
            let by = self.name(by);
            let text = text_of(blocks);
            self.push(entry.seq, format!("{by}: {text}"));
            return;
        }
        let from = self.name(entry.source.as_deref());
        let text = text_of(blocks);
        if !text.is_empty() {
            self.push(entry.seq, format!("{from}: {text}"));
        }
        for block in blocks {
            if let ContentBlock::Attachment(arrival) = block {
                self.push(entry.seq, format!("{from} attached {}", arrival.path));
            }
        }
    }

    fn assistant(&mut self, seq: Seq, output: &AssistantOutput) {
        let (prose, calls) = assistant(output);
        let speaker = self.reading.speaker().to_string();
        for text in prose {
            self.push(seq, format!("{speaker}: {text}"));
        }
        for (i, call) in calls.iter().enumerate() {
            let handle = call_label(seq, i, calls.len());
            match crate::process_monitor::ask_of_call(&calls, i) {
                Some((question, n, of)) => self.ask(&handle, &call.id.0, &question, n, of),
                None => {
                    let line = self.call_line(call);
                    self.push(handle, line);
                }
            }
        }
    }

    /// ``bash `cmd` — intent → allowed by you → ok, 12 lines, it saw 1``, with each
    /// escalation before the decision: `→ escalated by p to you: "<note>" →`.
    fn call_line(&self, call: &ToolCall) -> String {
        let id = call.id.0.as_str();
        let mut head = crate::record::names::tools::display_name(&call.name);
        if let Some(summary) = call_summary(call) {
            let _ = write!(head, " {}", code_span(&summary));
        }
        if let Some(intent) = call.intent() {
            let _ = write!(head, " — {}", flat(intent));
        }
        let mut parts = vec![head];
        for (_, escalation) in self.index.escalations(id) {
            parts.push(escalated(escalation, self.reading.me));
        }
        if self.reading.held == Some(id) {
            parts.push("waiting for a decision".to_string());
            return parts.join(" → ");
        }
        if let Some((_, decision)) = self.index.decision(id) {
            parts.push(self.decision_words(decision));
        }
        // A refused call's result is the harness's stand-in, not a run.
        if !self.index.refused(id) {
            parts.push(match self.index.result(id) {
                Some(result) => self.index.result_words(result),
                None => "running".to_string(),
            });
        }
        parts.join(" → ")
    }

    /// The question, then its reply wherever in the log the reply landed.
    fn ask(&mut self, handle: &str, call_id: &str, question: &str, n: usize, of: usize) {
        let place = match of {
            1 => String::new(),
            of => format!(" ({n} of {of})"),
        };
        let asked = format!("asked{place}: {question}");
        match self.index.result(call_id) {
            // An answer arrives as a submission; an interrupt or a malformed call
            // carries none (`audit.rs`).
            Some(reply) if reply.submission_id.is_some() => {
                self.push(handle, asked);
                let from = self.name(reply.source.as_deref());
                let text = result_text(reply);
                self.push(reply.seq, format!("{from}: {text}"));
            }
            Some(_) => self.push(handle, format!("{asked} → not answered")),
            None => self.push(handle, asked),
        }
    }

    /// A result whose call is before the window: its own line, under the call's handle.
    fn orphan_result(&mut self, entry: &'a LogEntry, call_id: &str) {
        let label = self.label(call_id, entry.seq);
        if self
            .index
            .call(call_id)
            .is_some_and(|call| call.ask.is_some())
        {
            let body = match entry.submission_id {
                Some(_) => format!(
                    "answered by {}: {}",
                    self.name(entry.source.as_deref()),
                    result_text(entry)
                ),
                None => "not answered".to_string(),
            };
            self.push(label, body);
            return;
        }
        if self.index.refused(call_id) {
            return;
        }
        let words = self.index.result_words(entry);
        self.push(label, format!("result: {words}"));
    }

    /// A verdict that landed in a later window than its result: a short line naming
    /// the call. Covered by the call's line or the result's when either is in view.
    fn orphan_verdict(&mut self, for_result: Seq, wire: &WireState) {
        let Some(result) = self.index.entry(for_result) else {
            return;
        };
        let Payload::ToolResult { call_id, raw, .. } = &result.payload else {
            return;
        };
        if self.window.contains(&for_result) || self.shown(&call_id.0) {
            return;
        }
        let label = self.label(&call_id.0, for_result);
        let body = saw(raw, Some(wire), true);
        self.push(label, format!("verdict: {body}"));
    }
}

/// A result entry's text: what an `ask`'s reply carries.
fn result_text(entry: &LogEntry) -> &str {
    match &entry.payload {
        Payload::ToolResult { raw, .. } => raw.content.trim_end(),
        _ => "",
    }
}

#[cfg(test)]
mod test;
