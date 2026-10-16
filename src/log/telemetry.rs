//! The telemetry family of log payloads (docs/log-and-projection.md): facts about how a
//! session ran rather than what it said. Each one projects to nothing, carries no prose a
//! reader is shown, and is skipped by every reader that renders a session, so a new kind
//! of event is a variant here and the readers' one `Payload::Telemetry(_)` arm covers it.
//! A reader that does look inside — masking, the operations topic, the web inspector —
//! matches the variant it wants.

use serde::{Deserialize, Serialize};

use super::{CallId, ColdEvent, PushRecord, RequestStage, ToolTelemetry};

/// One telemetry fact, recorded as `{"Telemetry": {"<Variant>": …}}`.
#[derive(Clone, Debug, Serialize, Deserialize)]
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
    /// One step of a moment's push: decided, held, escalated, cancelled, or the push
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
    /// A main-model consolidation check and its disposition (docs/consolidate.md).
    Consolidation {
        from: super::Seq,
        upto: super::Seq,
        context: super::Seq,
        tool_bytes: usize,
        outcome: String,
        account_bytes: usize,
    },
}

/// Where an interrupt came from. Both fields are client-asserted and unverified,
/// like a turn's `source`; a client that says nothing leaves both absent
/// (docs/control-api.md).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
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

impl Telemetry {
    /// The wire name of the variant — the `kind` of its log frame, under the
    /// `telemetry.` prefix so no kind collides with a state topic's name.
    pub fn kind(&self) -> &'static str {
        match self {
            Telemetry::Tool(_) => "telemetry.tool",
            Telemetry::CallStarted { .. } => "telemetry.call_started",
            Telemetry::RequestStage(_) => "telemetry.request_stage",
            Telemetry::Push(_) => "telemetry.push",
            Telemetry::Cold(_) => "telemetry.cold",
            Telemetry::CompactionRequested { .. } => "telemetry.compaction_requested",
            Telemetry::CompactionFailed { .. } => "telemetry.compaction_failed",
            Telemetry::InboxExpired { .. } => "telemetry.inbox_expired",
            Telemetry::SupervisorSet { .. } => "telemetry.supervisor_set",
            Telemetry::Interrupt(_) => "telemetry.interrupt",
            Telemetry::Consolidation { .. } => "telemetry.consolidation",
        }
    }
}
