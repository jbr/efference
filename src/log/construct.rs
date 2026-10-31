//! Building log entries and payloads (docs/log-and-projection.md).
//!
//! The constructors for code outside the append path and the stream assemblers, tests
//! included: a field added to [`LogEntry`] or to a [`Payload`] variant gets its default
//! here rather than at every site that builds one.

use super::*;

impl LogEntry {
    /// An entry at `seq` carrying `payload`, with zeroed metadata: turn 0, the epoch as
    /// its timestamp, and no submission id or source. The shape an offline rendering or
    /// a test builds; the log's append path records the real values. Each `with_*`
    /// method sets the field it names to the value given.
    pub fn new(seq: Seq, payload: Payload) -> Self {
        Self {
            seq,
            turn: 0,
            at: Timestamp(0),
            submission_id: None,
            source: None,
            payload,
        }
    }

    pub fn with_turn(mut self, turn: Turn) -> Self {
        self.turn = turn;
        self
    }

    pub fn with_at(mut self, at: Timestamp) -> Self {
        self.at = at;
        self
    }

    pub fn with_submission_id(mut self, submission_id: Option<String>) -> Self {
        self.submission_id = submission_id;
        self
    }

    pub fn with_source(mut self, source: Option<String>) -> Self {
        self.source = source;
        self
    }
}

impl Payload {
    /// A user turn of one text block.
    pub fn user(text: impl Into<String>) -> Self {
        Self::User(vec![ContentBlock::Text(text.into())])
    }

    /// A finalized assistant turn, projected as recorded.
    pub fn assistant(output: AssistantOutput) -> Self {
        Self::Assistant {
            output,
            excluded: None,
        }
    }

    /// An assistant turn kept **record-only** because of `why`: the log holds it for
    /// audit, and `project` skips it (docs/log-and-projection.md).
    pub fn record_only(output: AssistantOutput, why: Exclusion) -> Self {
        Self::Assistant {
            output,
            excluded: Some(why),
        }
    }

    /// A tool result payload for `call_id`.
    pub fn tool_result(call_id: impl Into<CallId>, raw: RawResult, wire: WireState) -> Self {
        Self::ToolResult {
            call_id: call_id.into(),
            raw,
            wire,
        }
    }

    /// The record of a deliberate archive, by `source` when the caller named one.
    pub fn archived(source: Option<String>) -> Self {
        Self::Archived { source }
    }
}
