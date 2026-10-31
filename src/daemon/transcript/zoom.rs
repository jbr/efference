//! The zoom (`GET /transcript?seq=8.2`, docs/control-api.md): one entry in full, as
//! plain text. The raw body is cut at the entry view's inline cap (`entry_view.rs`), so
//! a zoom answer is no larger than a window element; `GET /log/:seq` serves the rest, and
//! names itself only to a reader that can call it (`Reading::reads_by_route`).

use super::index::Index;
use super::*;
use crate::log::{ContentBlock, Seq, Spot, Telemetry};

/// A transcript handle: an entry's `seq`, or `seq.n` for the n-th call (1-based) of
/// an entry that issued several (`call_label`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Handle {
    pub(crate) seq: Seq,
    pub(crate) call: Option<usize>,
}

impl Handle {
    /// `8` or `8.2`, or what was wrong with it.
    pub(crate) fn parse(text: &str) -> Result<Self, String> {
        let bad =
            || format!("seq must be an entry's seq or a call's seq.n, like 8 or 8.2; got {text:?}");
        let (seq, call) = match text.split_once('.') {
            Some((seq, n)) => (seq, Some(n)),
            None => (text, None),
        };
        let seq = seq.parse().map_err(|_| bad())?;
        let call = call
            .map(|n| n.parse::<usize>().ok().filter(|n| *n >= 1).ok_or_else(bad))
            .transpose()?;
        Ok(Self { seq, call })
    }
}

impl std::fmt::Display for Handle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.call {
            Some(n) => write!(f, "{}.{n}", self.seq),
            None => write!(f, "{}", self.seq),
        }
    }
}

/// The entry `handle` names, in full, or why there is none.
pub(crate) fn zoom(log: &[LogEntry], handle: Handle, reading: &Reading) -> Result<String, String> {
    let index = Index::new(log);
    let Some(entry) = index.entry(handle.seq) else {
        return Err(match log.last() {
            Some(last) => format!(
                "no entry at seq {}: the log runs from seq {} to {}",
                handle.seq, log[0].seq, last.seq
            ),
            None => format!("no entry at seq {}: the log is empty", handle.seq),
        });
    };
    let mut out = Zoom {
        lines: vec![format!("{}, seq {handle}", reading.title())],
        index,
        reading,
    };
    match (&entry.payload, handle.call) {
        (Payload::Assistant { output, .. }, Some(n)) => {
            let (_, calls) = assistant(output);
            match calls.len() {
                // A lone call's handle is the bare seq; `.1` names it as well.
                1 if n == 1 => out.call(&calls[0], &call_label(entry.seq, 0, 1)),
                len if n <= len && len > 1 => {
                    out.call(&calls[n - 1], &call_label(entry.seq, n - 1, len));
                }
                0 => return Err(format!("seq {} issued no calls", handle.seq)),
                1 => {
                    return Err(format!(
                        "seq {0} issued one call, whose handle is {0}",
                        handle.seq
                    ));
                }
                len => {
                    return Err(format!(
                        "seq {} issued {}: {}",
                        handle.seq,
                        count(len, "call"),
                        handles(entry.seq, len)
                    ));
                }
            }
        }
        (_, Some(_)) => {
            return Err(format!(
                "seq {} is not an assistant turn; seq.n names one of the calls a turn issued",
                handle.seq
            ));
        }
        (Payload::Assistant { output, excluded }, None) => {
            out.turn(entry.seq, output, excluded.is_some());
        }
        (Payload::ToolResult { call_id, .. }, None) => match out.index.call(&call_id.0) {
            Some(call) => {
                let (call, label) = (call.call.clone(), call.handle.clone());
                out.call(&call, &label);
            }
            None => out.result(entry),
        },
        (Payload::User(blocks), None) => out.user(entry, blocks),
        (Payload::Ambient(note), None) => {
            let from = sender(Some(&note.source), reading.me);
            out.lines.push(format!("note from {from}:"));
            out.lines.push(note.body.trim_end().to_string());
        }
        (Payload::Summary(summary), None) => {
            out.lines
                .push(format!("summary of seq {}–{}:", summary.from, summary.upto));
            out.lines.push(summary.text.trim_end().to_string());
        }
        (Payload::Telemetry(Telemetry::GateDecision(decision)), None) => {
            let on = out.handle_of(&decision.call_id.0);
            out.lines.push(format!(
                "decision on {on}: {}",
                decided(
                    decision,
                    reading.me,
                    out.index.note(&decision.call_id.0).as_deref()
                )
            ));
        }
        (Payload::Telemetry(Telemetry::Escalation(escalation)), None) => {
            let on = out.handle_of(&escalation.call_id.0);
            out.lines.push(format!(
                "escalation of {on}: {}",
                escalated(escalation, reading.me)
            ));
        }
        (Payload::ResultVerdict { for_result, wire }, None) => {
            let mut line = format!("verdict on the result at seq {for_result}");
            if let Some(result) = out.index.entry(*for_result)
                && let Payload::ToolResult { call_id, raw, .. } = &result.payload
            {
                let _ = write!(
                    line,
                    " ({}): {}",
                    out.handle_of(&call_id.0),
                    saw(raw, Some(wire), true)
                );
            }
            out.lines.push(line);
        }
        (Payload::Spot(spot), None) => {
            let on = out.handle_of(&spot.call_id.0);
            out.lines.push(format!("the reviewer's verdict on {on}"));
            out.reviewer(Some(spot));
        }
        (Payload::Telemetry(telemetry), None) => {
            out.lines.push(format!("telemetry: {}", telemetry.kind()));
        }
        (_, None) => {
            for line in audit::lines(entry, &mut HashMap::new(), &mut HashSet::new()) {
                out.lines.push(format!("{}: {}", line.tag, line.body));
            }
        }
    }
    let mut text = out.lines.join("\n");
    text.push('\n');
    Ok(text)
}

/// `8.1, 8.2 and 8.3`: the handles of an entry's calls.
fn handles(seq: Seq, calls: usize) -> String {
    let all: Vec<String> = (0..calls).map(|i| call_label(seq, i, calls)).collect();
    match all.split_last() {
        Some((last, [])) => last.clone(),
        Some((last, init)) => format!("{} and {last}", init.join(", ")),
        None => String::new(),
    }
}

struct Zoom<'a, 'r> {
    lines: Vec<String>,
    index: Index<'a>,
    reading: &'r Reading<'r>,
}

impl<'a> Zoom<'a, '_> {
    fn handle_of(&self, call_id: &str) -> String {
        self.index.call(call_id).map_or_else(
            || "a call no entry issued".to_string(),
            |c| c.handle.clone(),
        )
    }

    fn user(&mut self, entry: &LogEntry, blocks: &[ContentBlock]) {
        let from = sender(entry.source.as_deref(), self.reading.me);
        self.lines.push(format!("{from}:"));
        for block in blocks {
            match block {
                ContentBlock::Text(text) => self.lines.push(text.trim_end().to_string()),
                ContentBlock::Attachment(arrival) => {
                    self.lines.push(format!("attached {}", arrival.path));
                }
                _ => {}
            }
        }
    }

    /// An assistant turn by its bare seq: its prose whole, then its one call in full,
    /// or each of several by its line.
    fn turn(&mut self, seq: Seq, output: &AssistantOutput, record_only: bool) {
        let (prose, calls) = assistant(output);
        let speaker = self.reading.speaker().to_string();
        for text in prose {
            self.lines.push(format!("{speaker}:"));
            self.lines.push(text);
        }
        if record_only {
            self.lines
                .push("this reply did not complete; the session never saw it".to_string());
        }
        match calls.as_slice() {
            [] => {}
            [call] => self.call(call, &seq.to_string()),
            calls => {
                for (i, call) in calls.iter().enumerate() {
                    let summary =
                        call_summary(call).map_or_default(|s| format!(" {}", code_span(&s)));
                    self.lines.push(format!(
                        "{}  {}{summary}",
                        call_label(seq, i, calls.len()),
                        crate::mcp::display_name(&call.name)
                    ));
                }
                self.lines.push(format!(
                    "read each call in full by its handle: {}",
                    handles(seq, calls.len())
                ));
            }
        }
    }

    /// One call in full, under its transcript handle.
    fn call(&mut self, call: &ToolCall, handle: &str) {
        let id = call.id.0.as_str();
        let ask = self.index.call(id).and_then(|c| c.ask.clone());
        self.lines.push(format!(
            "call {handle}: {}",
            crate::mcp::display_name(&call.name)
        ));
        if let Some(intent) = call.intent() {
            self.lines.push(format!("intent: {intent}"));
        }
        let mut input = call.input.clone();
        if let Value::Object(map) = &mut input {
            map.remove(crate::log::INTENT);
        }
        let pretty = serde_json::to_string_pretty(&input).unwrap_or_else(|_| input.to_string());
        self.lines.push(format!("input: {pretty}"));
        if let Some((question, n, of)) = ask {
            if of > 1 {
                self.lines.push(format!("question {n} of {of}: {question}"));
            }
            self.answer(id);
            return;
        }
        let spot = self.index.spot(id);
        self.reviewer(spot);
        self.decision(id, spot);
        match self.index.result(id) {
            Some(result) => self.result(result),
            None if self.reading.held == Some(id) => {}
            None => self
                .lines
                .push("result: none yet; it is running".to_string()),
        }
    }

    /// The spotter's verdict, in plain words.
    fn reviewer(&mut self, spot: Option<&Spot>) {
        let Some(spot) = spot else {
            self.lines.push("reviewer: not asked".to_string());
            return;
        };
        match spot.stance {
            Stance::Clear | Stance::ClearAlways => {
                self.lines.push("reviewer: cleared it".to_string());
            }
            Stance::Watch | Stance::Gate => {
                self.lines
                    .push("reviewer: held it for a decision".to_string());
                if let Some(reason) = crate::daemon::status::held_because(spot) {
                    self.lines.push(format!("held because: {reason}"));
                }
            }
        }
    }

    /// Whether the gate held the call, and who decided it and how.
    fn decision(&mut self, id: &str, spot: Option<&Spot>) {
        use crate::log::Approval;
        for (seq, escalation) in self.index.escalations(id) {
            let line = escalated(escalation, self.reading.me);
            self.lines.push(format!("{line} (seq {seq})"));
        }
        if self.reading.held == Some(id) {
            self.lines.push("decision: pending".to_string());
            return;
        }
        let Some((seq, decision)) = self.index.decision(id) else {
            let held = spot.is_some_and(|s| matches!(s.stance, Stance::Watch | Stance::Gate));
            self.lines.push(match held {
                true => "decision: none recorded".to_string(),
                false => "decision: none; the gate did not hold it".to_string(),
            });
            return;
        };
        let by = sender(decision.source.as_deref(), self.reading.me);
        let line = match &decision.decision {
            Approval::Approve => match self.index.note(id) {
                Some(note) => format!("decision: allowed by {by}, with \"{}\"", flat(&note)),
                None => format!("decision: allowed by {by}"),
            },
            Approval::Abort => format!("decision: denied by {by}, which ended the turn"),
            Approval::Redirect(message) => format!(
                "decision: denied by {by}; the session went on under the message: {}",
                message.trim_end()
            ),
            Approval::ConfigEdit { change, note } => {
                let mut line = format!(
                    "decision: granted by {by}: {}; the call ran only if the change cleared it",
                    change_words(change)
                );
                if !note.trim().is_empty() {
                    let _ = write!(line, " (note: {})", flat(note));
                }
                line
            }
        };
        self.lines.push(format!("{line} (seq {seq})"));
    }

    /// The result: its seq, outcome and size, what the session saw, the raw body
    /// capped, and its images.
    fn result(&mut self, result: &'a LogEntry) {
        let Payload::ToolResult { raw, .. } = &result.payload else {
            return;
        };
        let seq = result.seq;
        self.lines.push(format!(
            "result: seq {seq}, {}, {}",
            outcome(raw),
            size(raw.content.len())
        ));
        let seen = self.seen(result, raw);
        self.lines.push(seen);
        if raw.content.is_empty() {
            self.lines.push("raw: (empty)".to_string());
        } else {
            let cap = crate::daemon::entry_view::MAX_INLINE_RESULT_BYTES;
            let mut kept = raw.content.len().min(cap);
            while !raw.content.is_char_boundary(kept) {
                kept -= 1;
            }
            self.lines.push("raw:".to_string());
            self.lines
                .push(raw.content[..kept].trim_end_matches('\n').to_string());
            if kept < raw.content.len() {
                let mut cut = format!(
                    "cut: {} more of {}",
                    size(raw.content.len() - kept),
                    size(raw.content.len())
                );
                if self.reading.reads_by_route() {
                    let _ = write!(cut, "; GET /log/{seq} serves the whole result");
                }
                self.lines.push(cut);
            }
        }
        for (i, image) in raw.images.iter().enumerate() {
            let mut line = format!(
                "image {i}: {} {}×{}",
                image.media_type, image.width, image.height
            );
            if self.reading.reads_by_route() {
                let _ = write!(line, ", at GET /log/{seq}/images/{i}");
            }
            self.lines.push(line);
        }
    }

    /// `it saw: lines 1–3, 7 of 12: note`, and triage's remark on the intent.
    fn seen(&self, result: &'a LogEntry, raw: &RawResult) -> String {
        let verdict = match self.index.wire(result) {
            Some(WireState::Triaged(verdict)) => verdict,
            Some(WireState::Raw) => return "it saw: all of it".to_string(),
            Some(WireState::Elided) => return "it saw: none of it".to_string(),
            Some(WireState::Interrupted(interruption)) => {
                return format!(
                    "it saw: none of it, but {}",
                    crate::project::render_interruption(interruption)
                );
            }
            Some(WireState::TriageFailed { why }) => {
                return format!("it saw: none of it ({})", flat(why));
            }
            None | Some(WireState::Pending) => {
                return "it saw: nothing yet; no verdict has decided it".to_string();
            }
        };
        let d = selection(&raw.content, &verdict.keep);
        let (kept, total) = kept_of(raw, verdict);
        let mut line = if kept >= total {
            "it saw: all of it".to_string()
        } else if kept == 0 {
            "it saw: none of it".to_string()
        } else {
            let ranges: Vec<String> = d
                .ranges
                .iter()
                .map(|&(a, b)| match a == b {
                    true => a.to_string(),
                    false => format!("{a}–{b}"),
                })
                .collect();
            let noun = if kept == 1 { "line" } else { "lines" };
            format!("it saw: {noun} {} of {total}", ranges.join(", "))
        };
        if let Some(note) = &verdict.note {
            let _ = write!(line, ": {}", flat(note));
        }
        if let Some(feedback) = &verdict.intent_feedback {
            let _ = write!(line, "\nabout its intent: {}", flat(feedback));
        }
        line
    }

    /// An `ask`'s reply.
    fn answer(&mut self, id: &str) {
        match self.index.result(id) {
            Some(reply) if reply.submission_id.is_some() => {
                let from = sender(reply.source.as_deref(), self.reading.me);
                self.lines
                    .push(format!("answered at seq {} by {from}:", reply.seq));
                if let Payload::ToolResult { raw, .. } = &reply.payload {
                    self.lines.push(raw.content.trim_end().to_string());
                }
            }
            Some(reply) => self.lines.push(format!("not answered (seq {})", reply.seq)),
            None => self.lines.push("not answered yet".to_string()),
        }
    }
}

#[cfg(test)]
mod test;
