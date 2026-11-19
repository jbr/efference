//! What the operator, and any session supervising this one, said in a session, folded
//! from its log: the prose spotter's account of the session (docs/permissions.md).
//!
//! An utterance is a person's or a client's words: a `User` entry, or the reply to an
//! `ask`. Each carries the end of the session's own text it answers, so a short reply
//! has its referent. A `User` entry the harness wrote is left out: the kickoff a
//! `first-turn` config submits, a hand-off brief (written by the predecessor session),
//! and a redirect the loop's own permission review wrote. The fold reads the log alone,
//! in order, and an utterance depends only on the entries before it, so appending to a
//! log only ever appends to its utterances.

use std::collections::HashMap;

use serde_json::Value;

use crate::record::log::{
    Approval, CallId, ContentBlock, LogEntry, Payload, RawResult, Telemetry, UserContent,
};
use crate::record::names::sources;
use crate::record::names::tools::ASK;

/// How much of an utterance's text is kept: its first this-many characters.
pub const TEXT_CLIP: usize = 1500;

/// How much of the session's text an utterance answers is kept: its last this-many
/// characters.
pub const ANSWERS_CLIP: usize = 1200;

/// Who said an utterance.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Speaker {
    /// A person or client: no source, `web`, or `mcp[<name>]`.
    Operator,
    /// Another session, by id: a turn whose source is `session[<id>]`, which the
    /// spotter reads as the session supervising this one.
    Session(String),
}

impl Speaker {
    /// The speaker a `source` names.
    fn of(source: Option<&str>) -> Self {
        match source.and_then(sources::source_session) {
            Some(id) => Self::Session(id.to_string()),
            None => Self::Operator,
        }
    }
}

/// One thing the operator or a supervising session said, clipped.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Utterance {
    pub speaker: Speaker,
    /// What was said: its first [`TEXT_CLIP`] characters, with ` …` appended when
    /// clipped.
    pub text: String,
    /// The end of the session's text this answers — the prose of the session's last
    /// reply since the previous utterance, or the question an `ask` reply answers —
    /// as its last [`ANSWERS_CLIP`] characters, with `… ` prepended when clipped.
    /// `None` when the session wrote nothing since the previous utterance.
    pub answers: Option<String>,
}

impl Utterance {
    /// `text` by `speaker`, answering `answers`, each clipped.
    pub fn new(speaker: Speaker, text: &str, answers: Option<&str>) -> Self {
        Self {
            speaker,
            text: clip_head(text, TEXT_CLIP),
            answers: answers.map(|answers| clip_tail(answers, ANSWERS_CLIP)),
        }
    }
}

/// The utterances in `entries`, oldest first.
pub fn utterances(entries: &[LogEntry]) -> Vec<Utterance> {
    let mut out = Vec::new();
    // The prose of the session's last reply since the previous utterance.
    let mut answers: Option<String> = None;
    // The question of each `ask` call not yet answered, by call id. Looked up, never
    // iterated, so its order does not reach the output.
    let mut asks: HashMap<CallId, String> = HashMap::new();
    // The messages the permission review redirected with, in the order decided: each is
    // the text of a user turn the loop appends after the call's result.
    let mut reviewed: Vec<&str> = Vec::new();
    for entry in entries {
        match &entry.payload {
            Payload::Assistant {
                output,
                excluded: None,
            } => {
                let prose = output.text().join(" ");
                let prose = prose.trim();
                if !prose.is_empty() {
                    answers = Some(prose.to_string());
                }
                for call in output.tool_calls().unwrap_or_default() {
                    let question = call.input.get("question").and_then(Value::as_str);
                    if call.name == ASK
                        && let Some(question) = question
                    {
                        let question = question.trim().to_string();
                        asks.insert(call.id, question);
                    }
                }
            }
            Payload::Telemetry(Telemetry::GateDecision(decision)) => {
                if let Approval::Redirect(message) = &decision.decision
                    && decision
                        .source
                        .as_deref()
                        .is_some_and(sources::is_review_source)
                {
                    reviewed.push(message);
                }
            }
            Payload::User(blocks) => {
                if harness_written(entry, blocks, &mut reviewed) {
                    continue;
                }
                let text = user_text(blocks);
                if text.trim().is_empty() {
                    continue;
                }
                out.push(Utterance::new(
                    Speaker::of(entry.source.as_deref()),
                    &text,
                    answers.take().as_deref(),
                ));
            }
            // An `ask` reply: a submission produced it. Without one the question was
            // interrupted or malformed, and nobody answered it.
            Payload::ToolResult { call_id, raw, .. } if entry.submission_id.is_some() => {
                if let Some(question) = asks.remove(call_id) {
                    let text = reply_text(raw);
                    if !text.trim().is_empty() {
                        out.push(Utterance::new(
                            Speaker::of(entry.source.as_deref()),
                            &text,
                            Some(&question),
                        ));
                        answers = None;
                    }
                }
            }
            _ => {}
        }
    }
    out
}

/// Whether a `User` entry is one the harness wrote rather than a person or client: the
/// kickoff, a hand-off brief, or a redirect the permission review wrote. A review
/// redirect is the loop's own user turn (it has no submission) whose text is the message
/// of a pending review decision; it consumes that decision.
fn harness_written(entry: &LogEntry, blocks: &[ContentBlock], reviewed: &mut Vec<&str>) -> bool {
    if let Some(source) = entry.source.as_deref()
        && (source == sources::KICKOFF_SOURCE || sources::handoff_predecessor(source).is_some())
    {
        return true;
    }
    if entry.submission_id.is_some() {
        return false;
    }
    let [ContentBlock::Text(text)] = blocks else {
        return false;
    };
    match reviewed.iter().position(|message| message == text) {
        Some(at) => {
            reviewed.remove(at);
            true
        }
        None => false,
    }
}

/// A `User` entry's text: its text blocks, and a document's readable form.
fn user_text(blocks: &[ContentBlock]) -> String {
    let parts: Vec<String> = blocks
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text(text) => Some(text.trim().to_string()),
            ContentBlock::Document(document) => Some(document.readable().trim().to_string()),
            _ => None,
        })
        .filter(|text| !text.is_empty())
        .collect();
    parts.join(" ")
}

/// An `ask` reply's text: the readable form of its document when it has one, which
/// leaves out the attachment notice lines the result's `content` adds.
fn reply_text(raw: &RawResult) -> String {
    match raw.document.as_deref() {
        Some(document) => UserContent::readable(document),
        None => raw.content.clone(),
    }
}

/// The first `max` characters of `text`, trimmed, with ` …` appended when clipped.
fn clip_head(text: &str, max: usize) -> String {
    let text = text.trim();
    match text.char_indices().nth(max) {
        Some((end, _)) => format!("{} …", &text[..end]),
        None => text.to_string(),
    }
}

/// The last `max` characters of `text`, trimmed, with `… ` prepended when clipped.
fn clip_tail(text: &str, max: usize) -> String {
    let text = text.trim();
    let count = text.chars().count();
    if count <= max {
        return text.to_string();
    }
    let start = text
        .char_indices()
        .nth(count - max)
        .map_or(0, |(start, _)| start);
    format!("… {}", &text[start..])
}

#[cfg(test)]
mod test;
