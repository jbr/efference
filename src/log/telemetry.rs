//! The telemetry family of log payloads (docs/log-and-projection.md): facts about how a
//! session ran rather than what it said. Each one projects to nothing, carries no prose a
//! reader is shown, and is skipped by every reader that renders a session, so a new kind
//! of event is a variant here: those readers' wildcard arms skip it, and its frame's
//! `kind` comes from its name. A reader that does look inside — masking, the operations
//! topic, the web inspector — matches the variant it wants.

use serde::{Deserialize, Serialize};

use super::{Approval, CallId, ColdEvent, PushRecord, RequestStage, ToolTelemetry};

/// One telemetry fact, recorded as `{"Telemetry": {"<Variant>": …}}`.
#[derive(Clone, Debug, Serialize, Deserialize, strum::IntoStaticStr)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
// The variant's name is its log frame's `kind` (`Telemetry::kind`).
#[strum(serialize_all = "snake_case", prefix = "telemetry.")]
pub enum Telemetry {
    /// Per-call tool telemetry (docs/tools.md) — how long one call took, how it
    /// came out, and how big its result was, appended right after the
    /// [`ToolResult`] it describes.
    ///
    /// A separate entry rather than a field on [`ToolResult`], which is the
    /// shape [`Spot`] already uses for per-call audit: the pairing is by
    /// `call_id`, the payload is written by a different concern than the result,
    /// and [`ToolResult`] is constructed at dozens of sites (many of them a
    /// harness stand-in with no run to measure) that would each have to name a
    /// telemetry value they do not have.
    ///
    /// [`ToolResult`]: super::Payload::ToolResult
    /// [`Spot`]: super::Payload::Spot
    Tool(ToolTelemetry),
    /// A call's dispatch is about to reach the world: appended after the permission
    /// cascade clears it and before the tool runs. A call with this and no result may
    /// have had side effects, which is the one state a resumed session can't continue
    /// from.
    CallStarted { call_id: CallId },
    /// One stage of one upstream request — sent, an attempt that failed, first byte,
    /// ended, failed. Telemetry for "what was it doing, and since when" and the source
    /// of the `current_operations` topic (docs/control-api.md); the entry's `at` is the
    /// stage's time.
    RequestStage(RequestStage),
    /// One step of a moment's push: decided, held, released, cancelled, or the push
    /// services' answers (docs/web-push.md). The entry's `at` is the step's time.
    Push(PushRecord),
    /// One step of the cold check: the loop parked, a person released it, the park
    /// ended (docs/control-api.md). The entry's `at` is the step's time.
    Cold(ColdEvent),
    /// A compaction was asked for and accepted (`POST /loop/compact`, docs/summary.md):
    /// whether the operator gave up a warm cache for it, and the last seq the summary
    /// will cover. Its summarizing request follows as `RequestStage` entries with the
    /// `compaction` origin, then the compaction's `Summary` on success or a
    /// [`CompactionFailed`](Self::CompactionFailed). The compaction's state on
    /// `/state` is folded from these (docs/control-api.md).
    CompactionRequested { warm: bool, cut: super::Seq },
    /// The compaction asked for last did not land a summary: the error that ended it,
    /// a superseding turn included (docs/summary.md).
    CompactionFailed { error: String },
    /// A shown notification no triage verdict mentioned was dismissed by the harness
    /// after `shows` triages displayed it (docs/inbox.md). The entry resolves the
    /// notification the way a `Dismiss` would, and records that the model did not make
    /// the choice.
    InboxExpired { seq: super::Seq, shows: usize },
    /// The session's supervisor was set or cleared after creation
    /// (docs/permissions.md): the session now named, or `None` when cleared. The
    /// creator a session starts with is in the header and is not recorded here.
    SupervisorSet { supervisor: Option<String> },
    /// An interrupt reached the loop (`POST /loop/interrupt`, docs/control-api.md):
    /// who sent it and through which affordance, appended before the signal fires so
    /// it precedes the `failed: interrupted` stage it causes. Appended whether or not
    /// anything was running.
    Interrupt(InterruptOrigin),
    /// A continue was accepted (`POST /loop/continue`, an empty `message`;
    /// docs/control-api.md): who sent it. The loop takes another turn on the wire it
    /// already has, so this record is the only sign of who asked.
    Continue(ContinueOrigin),
    /// A decision on a held call was accepted (`PUT /approvals/:call_id`, the
    /// in-session approve; docs/control-api.md): which call, what was decided, and
    /// who decided it. Appended once the call is known to be the pending gate and
    /// before the decision reaches the loop, so it precedes the result or the
    /// redirect it causes. A redirect the loop's own permission review writes is
    /// recorded under `review[<level>]`; a spotter clear leaves none.
    GateDecision(GateDecision),
    /// A held call was escalated: whoever it was routed to passed it up without deciding
    /// it (`decide` with `escalate`, docs/permissions.md). It stays held, now routed to
    /// `to`. Appended when the escalation is accepted, so it precedes the decision that
    /// eventually resolves the call.
    Escalation(Escalation),
    /// A main-model consolidation check and its disposition (docs/consolidate.md).
    Consolidation {
        from: super::Seq,
        upto: super::Seq,
        context: super::Seq,
        tool_bytes: usize,
        outcome: String,
        account_bytes: usize,
    },
    /// The `tools[]` the session advertises, as sent: appended at boot, and at a resume
    /// when it differs from the last one recorded (docs/resumption.md). The array is
    /// kept as `Value`s, whose object keys serialize sorted, so equal tool sets are
    /// equal values. Offline readers (`dev wire`, the triage replay) rebuild the request
    /// from it instead of from current code.
    Tools { tools: Vec<serde_json::Value> },
}

/// Where an interrupt came from. Both fields are client-asserted and unverified,
/// like a turn's `source`; a client that says nothing leaves both absent
/// (docs/control-api.md).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
pub struct InterruptOrigin {
    /// Who interrupted, spelled as a turn's `source` is: `web`, `mcp[<name>]`,
    /// `session[<id>]`. Absent for the operator's own command line or TUI.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    /// The affordance that sent it: `stop-button`, `status-bar-stop`, `esc`,
    /// `control-cli`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub control: Option<String>,
}

/// Who sent an accepted continue ([`Telemetry::Continue`]), spelled as a turn's `source`
/// is: `web`, `mcp[<name>]`, `session[<id>]`; absent for the operator's own command line
/// or TUI (docs/control-api.md).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
pub struct ContinueOrigin {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
}

impl ContinueOrigin {
    /// This continue, sent by `source`.
    pub fn with_source(mut self, source: Option<String>) -> Self {
        self.source = source;
        self
    }
}

/// One accepted gate decision ([`Telemetry::GateDecision`]).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
pub struct GateDecision {
    /// The held call's `tool_use` id.
    pub call_id: CallId,
    /// The decision as the loop received it: approve, abort, redirect with its
    /// message, or a config edit with its change.
    pub decision: Approval,
    /// Who decided, spelled as a turn's `source` is: `web`, `mcp[<name>]`,
    /// `session[<id>]`. Client-asserted, like an interrupt's; absent for the
    /// operator's own command line or TUI.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    /// The submission id of the turn that carries the decider's note, for an allow
    /// sent with one (`decide`, docs/tools.md). The note is a user turn of its own,
    /// delivered with the call's result; this names it, so a reader of the log can tell
    /// it from a message the same sender wrote for another reason.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note_submission: Option<String>,
}

impl GateDecision {
    /// The record of `decision` on the call `call_id`, decided by `source`.
    pub fn new(call_id: impl Into<CallId>, decision: Approval, source: Option<String>) -> Self {
        Self {
            call_id: call_id.into(),
            decision,
            source,
            note_submission: None,
        }
    }

    /// This decision, with the note its decider sent beside it: the submission id of
    /// the note's turn.
    pub fn with_note_submission(mut self, submission_id: impl Into<String>) -> Self {
        self.note_submission = Some(submission_id.into());
        self
    }
}

/// One accepted escalation of a held call ([`Telemetry::Escalation`]).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
pub struct Escalation {
    /// The held call's `tool_use` id.
    pub call_id: CallId,
    /// Who escalated, spelled as a turn's `source` is: `mcp[<name>]`, `session[<id>]`.
    /// Absent for the operator's own command line.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    /// The session the call is routed to now; absent when it went to the operator.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to: Option<String>,
    /// The escalator's note to whoever decides next.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

impl Escalation {
    /// The record of `source` passing the call `call_id` to `to` (`None`: the
    /// operator), with `note`.
    pub fn new(
        call_id: impl Into<CallId>,
        source: Option<String>,
        to: Option<String>,
        note: Option<String>,
    ) -> Self {
        Self {
            call_id: call_id.into(),
            source,
            to,
            note,
        }
    }
}

impl Telemetry {
    /// The wire name of the variant — the `kind` of its log frame: the variant's name
    /// in snake case under the `telemetry.` prefix, so no kind collides with a state
    /// topic's name. Derived from the name, so a new variant brings its own.
    pub fn kind(&self) -> &'static str {
        self.into()
    }
}
