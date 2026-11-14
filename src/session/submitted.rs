//! A user turn as it was submitted to a session (docs/control-api.md): the type `POST
//! /v1/turns` fills, the session's inbox holds, and the agent loop reads.

use crate::protocol::turns::QueuedTurn;
use crate::record::log::attachment::Arrival;
use crate::record::log::{ContentBlock, Payload, ReferenceDefinition, UserContent, UserSegment};

/// A user turn as it arrived over the control channel: the submission id that
/// names this `POST /v1/turns` in the 202 body and on the resulting `User`
/// entry (docs/control-api.md), plus the turn's structured document. Both
/// body spellings — the legacy `text`+`attachments` and the structured
/// `content`+`references` — normalize into that one document, so the queue,
/// a recall, and the log all carry a single representation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SubmittedTurn {
    pub submission_id: String,
    /// The client's self-chosen label for who submitted this turn
    /// (docs/control-api.md): `web`, `mcp[<name>]`, or `session[<id>]`. `None` for
    /// a turn the operator submitted (the TUI) — the operator is the implicit
    /// default voice on the wire, so no prefix renders.
    pub source: Option<String>,
    /// The turn's structured document: ordered prose and reference occurrences
    /// plus their definitions (docs/log-and-projection.md). References enter
    /// only through the submission gate's validated path —
    /// `routes/http/turns.rs` resolves uploads, validates agent identities,
    /// and refuses filesystem targets before anything reaches here.
    pub content: UserContent,
}

impl SubmittedTurn {
    /// An operator turn: no source, no references. One of the two ways to
    /// build a submission, so a field added later lands here rather than at
    /// every site.
    pub fn new(text: impl Into<String>, submission_id: impl Into<String>) -> Self {
        Self {
            submission_id: submission_id.into(),
            source: None,
            content: UserContent::text(text),
        }
    }

    /// A turn carrying an already-built document. The submission gate is the
    /// only place a document with references enters a session's log; a client
    /// may resubmit the document a recall handed it, and the gate validates
    /// every submission again (docs/control-api.md).
    pub fn structured(content: UserContent, submission_id: impl Into<String>) -> Self {
        Self {
            submission_id: submission_id.into(),
            source: None,
            content,
        }
    }

    /// Attribute the turn to a non-operator speaker (docs/control-api.md).
    pub fn sourced(mut self, source: Option<String>) -> Self {
        self.source = source;
        self
    }

    /// Attach resolved arrivals as attachment references after the prose —
    /// the legacy body shape's attachments (docs/inbox.md). Ids mint `r1…`
    /// in arrival order, skipping any id the document already carries. An
    /// arrival the document already references by store name is skipped: one
    /// upload appears once (the gate refuses a second definition naming it), so
    /// a client that stages a recalled documented turn's chips alongside the
    /// held document sends them once.
    pub fn with_attachments(mut self, attachments: Vec<Arrival>) -> Self {
        let mut n = self.content.references.len();
        for arrival in attachments {
            if self
                .content
                .attachment_arrivals()
                .iter()
                .any(|present| present.name == arrival.name)
            {
                continue;
            }
            let mut id = format!("r{}", n + 1);
            while self.content.references.contains_key(&id) {
                n += 1;
                id = format!("r{}", n + 1);
            }
            self.content
                .content
                .push(UserSegment::Reference(id.clone()));
            self.content
                .references
                .insert(id, ReferenceDefinition::attachment(arrival));
            n += 1;
        }
        self
    }

    /// Nothing to deliver: no non-blank prose and no occurrences. A blank
    /// submit keeps the loop idling rather than landing an empty `User` entry
    /// on the wire.
    pub fn is_blank(&self) -> bool {
        self.content.is_blank()
    }

    /// The `User` entry this turn lands as: the document as the sole
    /// `Document` block (docs/log-and-projection.md). Flat `Text`/`Attachment`
    /// blocks belong to entries written before the document shape; every
    /// submission lands as a document.
    pub fn payload(&self) -> Payload {
        Payload::User(vec![ContentBlock::Document(self.content.clone())])
    }

    /// The structured body an `ask` reply keeps on its result entry — `None`
    /// for a plain answer, which keeps the logged bytes unchanged
    /// (docs/process-monitor.md).
    pub fn document(&self) -> Option<UserContent> {
        (!self.content.is_plain()).then(|| self.content.clone())
    }

    /// The turn's readable flat form: what a composer without reference
    /// support displays and splices (docs/control-api.md).
    pub fn readable(&self) -> String {
        self.content.readable()
    }

    /// The turn as an `ask`'s tool-result text (docs/process-monitor.md): the
    /// readable prose, then each attachment as the notice line a `User` entry
    /// projects, so a reply carries what a turn would have. The result entry
    /// keeps the structured body beside this ([`Self::document`]).
    pub fn reply_text(&self) -> String {
        let mut parts = Vec::new();
        let prose = self.content.readable();
        let trimmed = prose.trim();
        if !trimmed.is_empty() {
            parts.push(trimmed.to_string());
        }
        for arrival in self.content.attachment_arrivals() {
            parts.push(crate::record::log::attachment::turn_line(&arrival));
        }
        parts.join("\n")
    }
}

/// A pending turn as the `queued` topic, a recall and a withdrawal present it
/// (docs/control-api.md).
impl From<&SubmittedTurn> for QueuedTurn {
    fn from(turn: &SubmittedTurn) -> Self {
        QueuedTurn {
            submission_id: turn.submission_id.clone(),
            text: turn.readable(),
            document: turn.document(),
            attachments: turn.attachment_arrivals(),
        }
    }
}

impl SubmittedTurn {
    /// The resolved arrivals behind the turn's attachment references, in
    /// definition-id order — the legacy shape's `attachments` list, which a
    /// client re-stages on recall (docs/inbox.md).
    pub fn attachment_arrivals(&self) -> Vec<Arrival> {
        self.content.attachment_arrivals()
    }
}

#[cfg(test)]
mod test;
