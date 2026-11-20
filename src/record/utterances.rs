//! What was said in a session, folded from its log: the prose spotter's account of the
//! session's conversation (docs/permissions.md).
//!
//! An utterance is one message, whole, with its writer: a `User` entry a person or a
//! client sent, the reply to an `ask`, or the prose of one of the agent's own replies
//! with the question of any `ask` it made. A `User` entry the harness wrote is left out:
//! the kickoff a `first-turn` config submits, a hand-off brief (written by the
//! predecessor session), and a redirect the loop's own permission review wrote. The fold
//! reads the log alone, in order, and an utterance depends only on the entries before
//! it, so appending to a log only ever appends to its utterances.

use std::collections::HashSet;

use serde_json::Value;
use sha2::Digest;

use crate::record::log::{
    Approval, CallId, ContentBlock, LogEntry, Payload, RawResult, Telemetry, UserContent,
};
use crate::record::names::sources;
use crate::record::names::tools::ASK;

/// Who wrote an utterance.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Speaker {
    /// A person or client: no source, `web`, or `mcp[<name>]`.
    Operator,
    /// Another session's agent, by id: a turn whose source is `session[<id>]`, which the
    /// spotter reads as the agent supervising this one.
    Supervisor(String),
    /// The session's own agent: the prose of one of its replies.
    Agent,
}

impl Speaker {
    /// The speaker a `source` names, for an entry a submission produced.
    fn of(source: Option<&str>) -> Self {
        match source.and_then(sources::source_session) {
            Some(id) => Self::Supervisor(id.to_string()),
            None => Self::Operator,
        }
    }
}

/// One message of the conversation, whole.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Utterance {
    pub speaker: Speaker,
    /// What was written, trimmed.
    pub text: String,
}

impl Utterance {
    /// `text` by `speaker`, trimmed.
    pub fn new(speaker: Speaker, text: &str) -> Self {
        Self {
            speaker,
            text: text.trim().to_string(),
        }
    }
}

/// A session's utterances and the id the spotter's block tags them with.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Utterances {
    /// One value per session ([`tag_id`]): every tag around an utterance carries it.
    pub id: String,
    /// The utterances, oldest first.
    pub said: Vec<Utterance>,
}

/// The utterances in `entries`, the log of the session `session`, oldest first.
pub fn utterances(session: &str, entries: &[LogEntry]) -> Utterances {
    let mut said = Vec::new();
    // The `ask` calls made so far, by call id: a submitted result for one is a reply.
    // Looked up, never iterated, so its order does not reach the output.
    let mut asks: HashSet<CallId> = HashSet::new();
    // The messages the permission review redirected with, in the order decided: each is
    // the text of a user turn the loop appends after the call's result.
    let mut reviewed: Vec<&str> = Vec::new();
    for entry in entries {
        match &entry.payload {
            Payload::Assistant {
                output,
                excluded: None,
            } => {
                let mut text: Vec<String> = output
                    .text()
                    .into_iter()
                    .map(|text| text.trim().to_string())
                    .collect();
                for call in output.tool_calls().unwrap_or_default() {
                    if call.name != ASK {
                        continue;
                    }
                    if let Some(question) = call.input.get("question").and_then(Value::as_str) {
                        text.push(question.trim().to_string());
                    }
                    asks.insert(call.id);
                }
                text.retain(|text| !text.is_empty());
                push(&mut said, Speaker::Agent, &text.join("\n\n"));
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
                if !harness_written(entry, blocks, &mut reviewed) {
                    let speaker = Speaker::of(entry.source.as_deref());
                    push(&mut said, speaker, &user_text(blocks));
                }
            }
            // An `ask` reply: a submission produced it. Without one the question was
            // interrupted or malformed, and nobody answered it.
            Payload::ToolResult { call_id, raw, .. }
                if entry.submission_id.is_some() && asks.remove(call_id) =>
            {
                let speaker = Speaker::of(entry.source.as_deref());
                push(&mut said, speaker, &reply_text(raw));
            }
            _ => {}
        }
    }
    Utterances {
        id: tag_id(session, entries),
        said,
    }
}

/// Append `text` by `speaker`, unless it is blank.
fn push(said: &mut Vec<Utterance>, speaker: Speaker, text: &str) {
    if !text.trim().is_empty() {
        said.push(Utterance::new(speaker, text));
    }
}

/// The id a session's utterance tags carry: the first eight hex digits of a SHA-256 over
/// the session's id and the time its log's first entry was written. Neither changes once
/// the log has an entry, so every spotter call in the session renders the same id, a
/// host restart included, and two sessions render different ones.
fn tag_id(session: &str, entries: &[LogEntry]) -> String {
    let first = entries.first().map_or(0, |entry| entry.at.0);
    let digest = sha2::Sha256::digest(format!("{session}\n{first}").as_bytes());
    digest[..4]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
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
    parts.join("\n\n")
}

/// An `ask` reply's text: the readable form of its document when it has one, which
/// leaves out the attachment notice lines the result's `content` adds.
fn reply_text(raw: &RawResult) -> String {
    match raw.document.as_deref() {
        Some(document) => UserContent::readable(document),
        None => raw.content.clone(),
    }
}

#[cfg(test)]
mod test;
