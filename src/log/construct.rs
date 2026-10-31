//! Building log entries, payloads, and the records payloads carry
//! (docs/log-and-projection.md).
//!
//! The constructors for code outside the append path and the stream assemblers, tests
//! included: a field added to [`LogEntry`], to a [`Payload`] variant, or to a record such
//! as [`Spot`] or [`Triage`] gets its default here rather than at every site that builds
//! one.

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

    /// The final wire decision for the captured result at `for_result` (docs/triage.md).
    pub fn result_verdict(for_result: Seq, wire: WireState) -> Self {
        Self::ResultVerdict { for_result, wire }
    }

    /// The record of a deliberate archive, by `source` when the caller named one.
    pub fn archived(source: Option<String>) -> Self {
        Self::Archived { source }
    }
}

impl Telemetry {
    /// The call `call_id` is about to run (docs/log-and-projection.md).
    pub fn call_started(call_id: impl Into<CallId>) -> Self {
        Self::CallStarted {
            call_id: call_id.into(),
        }
    }
}

impl Spot {
    /// The spotter's `stance` on `call_id`, and why, as the decision model gave it: no
    /// model id, probed prefix, or axes recorded (docs/permissions.md). Each `with_*`
    /// method sets the field it names to the value given.
    pub fn new(call_id: impl Into<CallId>, stance: Stance, reason: impl Into<String>) -> Self {
        Self {
            call_id: call_id.into(),
            stance,
            reason: reason.into(),
            origin: Origin::Model,
            model: None,
            probed_prefix: None,
            axes: None,
        }
    }

    pub fn with_origin(mut self, origin: Origin) -> Self {
        self.origin = origin;
        self
    }

    pub fn with_model(mut self, model: Option<String>) -> Self {
        self.model = model;
        self
    }

    pub fn with_probed_prefix(mut self, probed_prefix: Option<String>) -> Self {
        self.probed_prefix = probed_prefix;
        self
    }

    pub fn with_axes(mut self, axes: Option<Axes>) -> Self {
        self.axes = axes;
        self
    }
}

impl Triage {
    /// The record of a triage of the result at `for_result` (`None` for an idle triage)
    /// whose keep the model wrote as `raw_keep`, decided by the model, with an empty
    /// inbox verdict and nothing surfaced, retried, thought, or shown (docs/triage.md).
    /// Each `with_*` method sets the field it names to the value given.
    pub fn new(for_result: Option<Seq>, raw_keep: impl Into<String>) -> Self {
        Self {
            for_result,
            origin: Origin::Model,
            raw_keep: raw_keep.into(),
            inbox: Vec::new(),
            surfaced: Vec::new(),
            retries: Vec::new(),
            thinking: Vec::new(),
            shown_through: None,
        }
    }

    pub fn with_origin(mut self, origin: Origin) -> Self {
        self.origin = origin;
        self
    }

    pub fn with_inbox(mut self, inbox: Vec<(Seq, Disposition)>) -> Self {
        self.inbox = inbox;
        self
    }

    pub fn with_surfaced(mut self, surfaced: Vec<String>) -> Self {
        self.surfaced = surfaced;
        self
    }

    pub fn with_retries(mut self, retries: Vec<String>) -> Self {
        self.retries = retries;
        self
    }

    pub fn with_thinking(mut self, thinking: Vec<String>) -> Self {
        self.thinking = thinking;
        self
    }

    pub fn with_shown_through(mut self, shown_through: Option<Seq>) -> Self {
        self.shown_through = shown_through;
        self
    }
}

impl ConfigEdit {
    /// An edit in `state` making `change`, resolving no proposal and carrying no note
    /// (docs/permissions.md). Each `with_*` method sets the field it names to the value
    /// given.
    pub fn new(state: EditState, change: ConfigChange) -> Self {
        Self {
            state,
            resolves: None,
            change,
            note: String::new(),
        }
    }

    pub fn with_resolves(mut self, resolves: Option<Seq>) -> Self {
        self.resolves = resolves;
        self
    }

    pub fn with_note(mut self, note: String) -> Self {
        self.note = note;
        self
    }
}

impl Title {
    /// A title `text` written by `author` (docs/session-title.md).
    pub fn new(text: impl Into<String>, author: TitleAuthor) -> Self {
        Self {
            text: text.into(),
            author,
        }
    }
}

impl Resumed {
    /// A reopening `gap_ms` after the log's last entry that set nothing aside: no lost
    /// processes, no skipped entries, no pending calls, and the same tools
    /// (docs/resumption.md). Each `with_*` method sets the field it names to the value
    /// given.
    pub fn new(gap_ms: u64) -> Self {
        Self {
            gap_ms,
            lost_processes: Vec::new(),
            resumes_after: None,
            pending_calls: Vec::new(),
            tools_changed: ToolsChange::default(),
        }
    }

    pub fn with_lost_processes(mut self, lost_processes: Vec<String>) -> Self {
        self.lost_processes = lost_processes;
        self
    }

    pub fn with_resumes_after(mut self, resumes_after: Option<Seq>) -> Self {
        self.resumes_after = resumes_after;
        self
    }

    pub fn with_pending_calls(mut self, pending_calls: Vec<CallId>) -> Self {
        self.pending_calls = pending_calls;
        self
    }

    pub fn with_tools_changed(mut self, tools_changed: ToolsChange) -> Self {
        self.tools_changed = tools_changed;
        self
    }
}
