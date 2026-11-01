//! A user turn as it was submitted to a session (docs/control-api.md): the type `POST
//! /v1/turns` fills, the session's inbox holds, and the agent loop reads.

use crate::protocol::turns::QueuedTurn;
use crate::record::log::ContentBlock;

/// A user turn as it arrived over the control channel: the text plus the
/// submission id that names this `POST /v1/turns` in the 202 body and on the
/// resulting `User` entry (docs/control-api.md). The id is adopted from the
/// client when it minted one, daemon-minted otherwise, so every submitted
/// turn carries one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SubmittedTurn {
    pub text: String,
    pub submission_id: String,
    /// The client's self-chosen label for who submitted this turn
    /// (docs/control-api.md): `web`, `mcp[<name>]`, or `session[<id>]`. `None` for
    /// a turn the operator submitted (the TUI) — the operator is the implicit
    /// default voice on the wire, so no prefix renders.
    pub source: Option<String>,
    /// Files the submitting composer attached, already in the session's store
    /// (docs/inbox.md). Each lands on the `User` entry as a
    /// [`ContentBlock::Attachment`] after the text.
    pub attachments: Vec<crate::record::log::attachment::Arrival>,
}

impl SubmittedTurn {
    /// An operator turn: no source, no attachments. The one way to build a
    /// submission, so a field added later lands here rather than at every site.
    pub fn new(text: impl Into<String>, submission_id: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            submission_id: submission_id.into(),
            source: None,
            attachments: Vec::new(),
        }
    }

    /// Attribute the turn to a non-operator speaker (docs/control-api.md).
    pub fn sourced(mut self, source: Option<String>) -> Self {
        self.source = source;
        self
    }

    pub fn with_attachments(
        mut self,
        attachments: Vec<crate::record::log::attachment::Arrival>,
    ) -> Self {
        self.attachments = attachments;
        self
    }

    /// Nothing to deliver: blank text and no attachments. A blank submit keeps the
    /// loop idling rather than landing an empty `User` entry on the wire.
    pub fn is_blank(&self) -> bool {
        self.text.trim().is_empty() && self.attachments.is_empty()
    }

    /// The `User` entry's content: the text when there is any, then each attachment.
    pub fn blocks(&self) -> Vec<ContentBlock> {
        let text = (!self.text.trim().is_empty()).then(|| ContentBlock::Text(self.text.clone()));
        text.into_iter()
            .chain(
                self.attachments
                    .iter()
                    .cloned()
                    .map(ContentBlock::Attachment),
            )
            .collect()
    }
}

impl SubmittedTurn {
    /// The turn as an `ask`'s tool-result text (docs/process-monitor.md): the prose, then
    /// each attachment as the notice line a `User` entry projects, so a reply carries
    /// what a turn would have.
    pub fn reply_text(&self) -> String {
        let text = self.text.trim();
        (!text.is_empty())
            .then(|| text.to_string())
            .into_iter()
            .chain(
                self.attachments
                    .iter()
                    .map(crate::record::log::attachment::turn_line),
            )
            .collect::<Vec<_>>()
            .join("\n")
    }
}

/// A pending turn as the `queued` topic, a recall and a withdrawal present it
/// (docs/control-api.md).
impl From<&SubmittedTurn> for QueuedTurn {
    fn from(turn: &SubmittedTurn) -> Self {
        QueuedTurn {
            submission_id: turn.submission_id.clone(),
            text: turn.text.clone(),
            attachments: turn.attachments.clone(),
        }
    }
}
