//! The trajectory digest: a session's log folded to one line per tool call
//! (docs/digest.md).
//!
//! Each call renders as its tool name, the paths it named, its `intent`, and the note
//! triage wrote for its result; operator turns render verbatim, and the prose that
//! ended a turn renders as the agent's reply. Bodies — arguments that are not paths,
//! result content — are left out, so the digest's size follows the number of calls
//! rather than the volume of work they did. It is a deterministic fold over the log and
//! asks no model anything.

use crate::log::{CallId, ContentBlock, LogEntry, Payload, RawResult, Seq, ToolCall, WireState};
use crate::project::Reader;
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::fmt::Write as _;

/// How a digest renders.
#[derive(Clone, Copy, Debug, Default)]
pub struct Options {
    /// Include only entries with `seq <= upto`; `None` takes the whole log.
    pub upto: Option<Seq>,
    /// Prefix each block with the seq of the entry it came from, so a reader can
    /// fetch the entry behind a line.
    pub seqs: bool,
    /// The projection the digest is read for (docs/summary.md): a summary
    /// addressed to the other reader is left out, as it is from that reader's wire.
    pub reader: Reader,
    /// Mark the point a summary's account ends (docs/summary.md): [`CUT`] goes
    /// before the first block from an entry after this seq, and only if one renders.
    pub cut: Option<Seq>,
    /// Render each call with its full arguments and its result as the agent read it
    /// (the kept lines and note) instead of as one line: the separate triage's triaged
    /// tail (docs/triage.md).
    pub verbatim: bool,
}

/// The line [`Options::cut`] places: everything after it stays on the wire verbatim
/// after the summary, so the model reads it but does not summarize it.
pub const CUT: &str = "### cut: your account ends here; what follows stays verbatim\n\n";

/// Fold `entries` to the digest text.
pub fn render(entries: &[LogEntry], options: Options) -> String {
    // Seqs only grow along a log, so the entries at or below `upto` are a prefix of it.
    let end = options.upto.map_or(entries.len(), |upto| {
        entries
            .iter()
            .position(|e| e.seq > upto)
            .unwrap_or(entries.len())
    });
    Fold::new(&entries[..end]).render(0..end, options)
}

/// A log read once for rendering in spans: which entries never reached the model, and
/// the wire state each held result's verdict gave it. A reader that renders the same
/// log piecewise (the separate triage's text view, docs/triage.md) renders each span
/// from here rather than re-reading the whole log per span.
///
/// A span renders a call only if it holds the call's assistant entry, so a span
/// boundary belongs before an assistant entry; the concatenated spans then equal the
/// whole render.
pub struct Fold<'a> {
    entries: &'a [LogEntry],
    hidden: HashSet<Seq>,
    verdicts: HashMap<Seq, &'a WireState>,
}

impl<'a> Fold<'a> {
    pub fn new(entries: &'a [LogEntry]) -> Self {
        // Entries a resumption set aside never reached the model, and a result held
        // `Pending` takes the wire state its later verdict gave it.
        let mut hidden: HashSet<Seq> = HashSet::new();
        let mut verdicts: HashMap<Seq, &WireState> = HashMap::new();
        for entry in entries {
            match &entry.payload {
                Payload::Resumed(resumed) => hidden.extend(resumed.skipped(entry.seq)),
                Payload::ResultVerdict { for_result, wire } => {
                    verdicts.insert(*for_result, wire);
                }
                _ => {}
            }
        }
        Fold {
            entries,
            hidden,
            verdicts,
        }
    }

    /// Leave these entries out of every span, as a resumption's skipped entries are.
    pub fn hide(&mut self, seqs: impl IntoIterator<Item = Seq>) {
        self.hidden.extend(seqs);
    }

    /// The digest text of `entries[span]`. [`Options::upto`] is not read here: the
    /// fold's entries are already the ones to render.
    pub fn render(&self, span: std::ops::Range<usize>, options: Options) -> String {
        let mut out = String::new();
        let mut calls: HashMap<CallId, (Seq, ToolCall)> = HashMap::new();
        let mut asks: HashSet<CallId> = HashSet::new();
        let mut cut_placed = false;
        for entry in self.entries[span]
            .iter()
            .filter(|e| !self.hidden.contains(&e.seq))
        {
            let seq = entry.seq;
            let before = out.len();
            match &entry.payload {
                Payload::User(blocks) => {
                    let mut text = Vec::new();
                    for block in blocks {
                        match block {
                            ContentBlock::Text(t) if !t.trim().is_empty() => {
                                text.push(t.trim().to_string());
                            }
                            ContentBlock::Attachment(arrival) => {
                                text.push(format!("(attached {})", arrival.path));
                            }
                            _ => {}
                        }
                    }
                    if !text.is_empty() {
                        let who = entry.source.as_deref().unwrap_or("operator");
                        heading(&mut out, who, seq, options);
                        let _ = writeln!(out, "{}\n", text.join("\n"));
                    }
                }
                Payload::Assistant {
                    output,
                    excluded: None,
                } => {
                    let turn_calls = output.tool_calls().unwrap_or_default();
                    // Every turn's prose, mid-turn narration included: it is where the
                    // agent says what it took from the operator and what it is about to
                    // do, which the call lines alone do not carry (docs/digest.md).
                    let prose = output.text().join("\n").trim().to_string();
                    if !prose.is_empty() {
                        heading(&mut out, "agent", seq, options);
                        let _ = writeln!(out, "{prose}\n");
                    }
                    for (i, call) in turn_calls.iter().enumerate() {
                        if let Some((question, _, _)) =
                            crate::process_monitor::ask_of_call(&turn_calls, i)
                        {
                            asks.insert(call.id.clone());
                            heading(&mut out, "agent asked", seq, options);
                            let _ = writeln!(out, "{question}\n");
                        } else {
                            calls.insert(call.id.clone(), (seq, call.clone()));
                        }
                    }
                }
                Payload::ToolResult { call_id, raw, wire } => {
                    // An ask's result is the reply when a submission produced it; with
                    // none, the question was interrupted or malformed and stayed open.
                    if asks.contains(call_id) {
                        if entry.submission_id.is_some() {
                            let who = entry.source.as_deref().unwrap_or("operator");
                            heading(&mut out, &format!("{who} replied"), seq, options);
                            let _ = writeln!(out, "{}\n", raw.content.trim());
                        } else {
                            let _ = writeln!(out, "(not answered)\n");
                        }
                    } else if let Some((call_seq, call)) = calls.remove(call_id) {
                        let wire = self.verdicts.get(&seq).copied().unwrap_or(wire);
                        if options.seqs {
                            let _ = write!(out, "{call_seq} ");
                        }
                        if options.verbatim {
                            verbatim_call(&mut out, &call, raw, wire);
                        } else {
                            let _ = match call.intent() {
                                Some(intent) => writeln!(out, "• {} {intent}", tag(&call)),
                                None => writeln!(out, "• {}", tag(&call)),
                            };
                            let _ = writeln!(out, "    → {}", outcome(raw.is_error, wire));
                        }
                    }
                }
                Payload::Summary(summary) if !options.reader.applies(summary.audience) => {}
                Payload::Summary(summary) => {
                    heading(&mut out, &summary.label().replace('#', ""), seq, options);
                    let _ = writeln!(out, "{}\n", summary.text.trim());
                }
                _ => {}
            }
            // Placed after the fact, so a tail entry that renders nothing (a verdict, a
            // record-only failure) cannot leave the marker with nothing under it.
            let past_cut = options.cut.is_some_and(|cut| seq > cut);
            if past_cut && !cut_placed && out.len() > before {
                out.insert_str(before, CUT);
                cut_placed = true;
            }
        }
        out
    }
}

/// A call in full: its name and arguments, then its result as the agent read it,
/// indented under it (docs/triage.md). The `[seq]` handle the agent's wire carries is
/// left out, as the digest leaves seqs out unless asked.
fn verbatim_call(out: &mut String, call: &ToolCall, raw: &RawResult, wire: &WireState) {
    let _ = writeln!(out, "• {} {}", call.name, call.input);
    let (content, is_error) = crate::project::result_content(raw, wire);
    if is_error {
        let _ = writeln!(out, "    (error)");
    }
    if !raw.images.is_empty() && matches!(wire, WireState::Raw | WireState::Triaged(_)) {
        let n = raw.images.len();
        let noun = if n == 1 { "image" } else { "images" };
        let _ = writeln!(out, "    ({n} {noun}, not shown here)");
    }
    for line in content.lines() {
        if line.is_empty() {
            out.push('\n');
        } else {
            let _ = writeln!(out, "    {line}");
        }
    }
    out.push('\n');
}

fn heading(out: &mut String, who: &str, seq: Seq, options: Options) {
    if options.seqs {
        let _ = writeln!(out, "### {who} (seq {seq})");
    } else {
        let _ = writeln!(out, "### {who}");
    }
}

/// `[name path, path]`: the call's tool and the paths it named, read from the
/// per-tool key the permission classifier reads (docs/tools.md).
fn tag(call: &ToolCall) -> String {
    let paths = crate::policy::path_spec(&call.name)
        .and_then(|(key, _, _)| call.input.get(key))
        .map(|value| match value {
            Value::String(s) => vec![s.as_str()],
            Value::Array(items) => items.iter().filter_map(Value::as_str).collect(),
            _ => vec![],
        })
        .unwrap_or_default();
    if paths.is_empty() {
        format!("[{}]", call.name)
    } else {
        format!("[{} {}]", call.name, paths.join(", "))
    }
}

/// The triaged note, or what stood in for one.
fn outcome(is_error: bool, wire: &WireState) -> String {
    let note = match wire {
        WireState::Triaged(d) => d.note.as_deref().map(str::trim).filter(|n| !n.is_empty()),
        _ => None,
    };
    match (note, is_error) {
        (Some(note), false) => note.to_string(),
        (Some(note), true) => format!("(error) {note}"),
        (None, true) => "(error, no note)".to_string(),
        (None, false) => "(no note)".to_string(),
    }
}

#[cfg(test)]
mod test;
