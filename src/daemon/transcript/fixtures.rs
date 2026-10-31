//! Logs for the conversation and zoom tests, built through the production appenders.

use crate::log::{
    Approval, AssistantOutput, ContentBlock, GateDecision, Origin, Payload, Seq, Spot, Stance,
    Telemetry, ToolCall,
};
use crate::persist::Log;

/// An in-memory log: `seq` is the append index, as in a recorded one.
pub(super) struct Fixture(pub(super) Log);

impl Fixture {
    pub(super) fn new() -> Self {
        Self(Log::in_memory())
    }

    /// Append `payload`; returns its seq.
    pub(super) fn add(&mut self, payload: Payload) -> Seq {
        self.0.append(1, payload).unwrap()
    }

    /// A user turn as a submission from `source` (`None`: the operator).
    pub(super) fn user(&mut self, text: &str, source: Option<&str>) -> Seq {
        self.submitted(Payload::User(vec![ContentBlock::Text(text.into())]), source)
    }

    /// `payload` as a submission from `source`: a user turn or an `ask`'s reply.
    pub(super) fn submitted(&mut self, payload: Payload, source: Option<&str>) -> Seq {
        let id = format!("sub-{}", self.0.entries().len());
        self.0
            .append_submission(1, payload, Some(id), source.map(str::to_string))
            .unwrap()
    }

    /// A user turn submitted under the id `submission_id`, which a decision's note
    /// names.
    pub(super) fn user_as(&mut self, text: &str, source: Option<&str>, submission_id: &str) -> Seq {
        self.0
            .append_submission(
                1,
                Payload::User(vec![ContentBlock::Text(text.into())]),
                Some(submission_id.to_string()),
                source.map(str::to_string),
            )
            .unwrap()
    }

    /// A redirect's message as the loop appends it: no submission, under its decider's
    /// `source` (`agent/landing.rs`).
    pub(super) fn steer(&mut self, text: &str, source: Option<&str>) -> Seq {
        self.0
            .append_submission(
                1,
                Payload::User(vec![ContentBlock::Text(text.into())]),
                None,
                source.map(str::to_string),
            )
            .unwrap()
    }

    pub(super) fn entries(&self) -> &[crate::log::LogEntry] {
        self.0.entries()
    }
}

pub(super) fn turn(text: &str, calls: Vec<ToolCall>) -> Payload {
    let text = match text {
        "" => vec![],
        text => vec![ContentBlock::Text(text.into())],
    };
    Payload::Assistant {
        output: AssistantOutput::from_legacy_messages(vec![], text, calls),
        excluded: None,
    }
}

pub(super) fn bash(id: &str, command: &str, intent: &str) -> ToolCall {
    ToolCall::new(
        crate::bash::NAME,
        id,
        serde_json::json!({ "command": command }),
    )
    .with_intent(intent)
}

pub(super) fn ask(id: &str, question: &str) -> ToolCall {
    ToolCall::new(
        crate::log::ASK,
        id,
        serde_json::json!({ "question": question }),
    )
}

/// A prose-rung spotter verdict, as the cascade logs it before the gate.
pub(super) fn spot(id: &str, stance: Stance, reason: &str) -> Payload {
    Payload::Spot(Spot {
        call_id: id.into(),
        stance,
        reason: reason.into(),
        origin: Origin::Model,
        model: None,
        probed_prefix: None,
        axes: None,
    })
}

/// A gate decision's record, as `resolve_approval` appends it.
pub(super) fn decision(id: &str, decision: Approval, source: Option<&str>) -> Payload {
    Payload::Telemetry(Telemetry::GateDecision(GateDecision::new(
        id,
        decision,
        source.map(str::to_string),
    )))
}

/// An escalation's record, as `Session::escalate` appends it: `source` passed the call
/// to the session `to`, or to the operator when `None`.
pub(super) fn escalation(
    id: &str,
    source: Option<&str>,
    to: Option<&str>,
    note: Option<&str>,
) -> Payload {
    Payload::Telemetry(Telemetry::Escalation(crate::log::Escalation::new(
        id,
        source.map(str::to_string),
        to.map(str::to_string),
        note.map(str::to_string),
    )))
}

/// An allow sent with a note, whose turn was submitted under `note_submission`.
pub(super) fn allow_noted(id: &str, source: Option<&str>, note_submission: &str) -> Payload {
    Payload::Telemetry(Telemetry::GateDecision(
        GateDecision::new(id, Approval::Approve, source.map(str::to_string))
            .with_note_submission(note_submission),
    ))
}

/// `n` numbered lines.
pub(super) fn lines(n: usize) -> String {
    (1..=n)
        .map(|i| format!("line {i}"))
        .collect::<Vec<_>>()
        .join("\n")
}
