//! The append-only annotated log — the source of truth (docs/README.md, docs/log-and-projection.md).
//!
//! [`Payload`] is the whole vocabulary of what may be recorded; [`WireState`] is
//! the whole vocabulary of what a recorded tool result may show the model.
//! Everything downstream reads these two types, so a new kind of fact about a
//! session is a variant here before it is anything else.

use serde::{Deserialize, Serialize};
use serde_json::Value;

mod curation;
pub use curation::result_verdicts;

mod assistant;
mod cold;
mod completions;
mod config_edit;
mod process;
mod push;
mod request;
mod result;
mod spot;
pub mod stops;
mod summary;
mod telemetry;
mod tool_telemetry;
mod triage;
mod upstream;
mod wire_state;
mod worktree;

#[cfg(test)]
mod test;
pub use assistant::{
    AssistantOutput, MessagesOutputBlock, ResponsesMessageContent, ResponsesOutputItem,
    ResponsesPhase, ResponsesReasoningSummary, ResponsesReasoningText,
};
pub use cold::{ColdEvent, ColdReason, ReleasedBy, UnparkCause};
pub use completions::CompletionsMessage;
pub use config_edit::{ConfigChange, ConfigEdit, EditState};
pub use process::{ExitOutcome, ProcessEvent, ProcessEventKind, ProcessExit, ProcessSpawn};
pub use push::{
    AttentiveClient, DeliveryOutcome, DeviceDelivery, MomentKind, PushCancel, PushEvent, PushRecord,
};
pub use request::{RequestOrigin, RequestStage, RequestStageKind};
pub use result::{ContentElision, RawResult, ResultImage};
pub use spot::{Spot, Stance};
pub use stops::Stop;
pub use summary::{Audience, MadeBy, Summary};
pub use telemetry::{InterruptOrigin, Telemetry};
pub use tool_telemetry::{BashTelemetry, EditTelemetry, ToolOutcome, ToolTelemetry};
pub use triage::{Origin, Triage};
pub use upstream::{
    CAPTURED_HEADERS, FRAME_CAPTURE_CAP, StreamDiagnostics, UpstreamFailureRecord,
    is_captured_header,
};
pub use wire_state::{Keep, Triaged, WireState};
pub use worktree::{Checkout, WorktreeEvent, WorktreeEventKind};

/// Monotonic per-entry sequence number; the address space [`project`] reads over.
///
/// [`project`]: crate::project::project
pub type Seq = u64;

/// Which agent turn produced an entry. Metadata only — never projected.
pub type Turn = u32;

/// Wall-clock millis since the Unix epoch. **Log metadata only**: timestamps are
/// never projected onto the wire, so their non-determinism cannot affect cache
/// stability (docs/log-and-projection.md).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Timestamp(pub u64);

impl Timestamp {
    /// The wall clock, now. The single place a wall-clock reading enters the process, so
    /// the non-determinism has one door rather than several.
    ///
    /// Safe for anything that is never projected — log metadata, and the absolute
    /// instants control-API state representations carry so clients can compute elapsed
    /// without the daemon re-emitting a ticking field (docs/control-api.md). It must not
    /// reach the wire projection, where any per-process variation breaks the cache prefix
    /// (docs/log-and-projection.md).
    pub fn now() -> Self {
        Self(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("system clock before Unix epoch")
                .as_millis() as u64,
        )
    }
}

/// An Anthropic tool-use id (`toolu_…`) and its matching `tool_result` id.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct CallId(pub String);

impl From<&str> for CallId {
    fn from(id: &str) -> Self {
        Self(id.into())
    }
}

impl From<&String> for CallId {
    fn from(id: &String) -> Self {
        Self(id.clone())
    }
}

impl From<String> for CallId {
    fn from(id: String) -> Self {
        Self(id)
    }
}

/// One immutable, ordered record in the log.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LogEntry {
    pub seq: Seq,
    pub turn: Turn,
    pub at: Timestamp,
    /// The submission id of the `POST /v1/turns` this entry realized
    /// (docs/control-api.md): client-minted when the body carried one,
    /// daemon-minted otherwise, `None` on entries the loop originates itself
    /// (redirects, handoff notes). **Log metadata only** — never projected,
    /// like `at` and `turn` (docs/log-and-projection.md). Clients key delivery
    /// reconciliation on it; it also records which socket submission produced
    /// the entry, for audit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub submission_id: Option<String>,
    /// Who submitted this entry, when that is not the operator
    /// (docs/control-api.md): the client's self-chosen label — `web`,
    /// `mcp[<name>]`, or `agent[<id>]`. `None` on operator turns and on every
    /// entry the loop originates itself. Unlike `submission_id`, this **is**
    /// projected: a `User` entry with a source opens its wire block with a
    /// `source:` line (docs/log-and-projection.md). Skipped when `None`, so
    /// unsourced entries and pre-existing logs read back and serialize
    /// unchanged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    pub payload: Payload,
}

/// The content of a log entry. Immutable once appended; curation never mutates
/// a payload's `raw`, only flips a [`WireState`] (docs/README.md).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum Payload {
    /// Prose (and, later, richer blocks) authored by the user / operator.
    User(Vec<ContentBlock>),
    /// One model turn in its provider dialect's ordered output vocabulary.
    ///
    /// Both Messages content blocks and Responses output items are ordered arrays on
    /// their native wires. Keeping that order in the record makes projection a copy of
    /// provider-authored structure rather than a reconstruction from grouped fields
    /// (docs/log-and-projection.md).
    Assistant {
        output: AssistantOutput,
        /// `Some` = the upstream stream died before finalizing (no `message_stop` /
        /// `[DONE]`, an in-stream `error` event, or a mid-stream transport failure) —
        /// or, for an entry with no content at all, the request itself failed and this
        /// records the error body durably (docs/transport.md). Its `why` says which:
        /// the provider's error verbatim when one arrived, a harness-written note for
        /// a silent close. One path attaches more — a stream that finalizes empty
        /// carries [`StreamDiagnostics`], because nothing else about it survives
        /// anywhere. Such an entry is **record-only**: the partial (often the
        /// only copy anywhere) stays in the log for audit, but [`project`] skips the
        /// entry entirely — the provider has no record of the generation and doesn't
        /// stand behind it, so it must not enter the trajectory. The assistant-side
        /// analogue of a [`WireState`] verdict: the mark lives in the entry because
        /// projection is deterministic over the log alone (docs/log-and-projection.md).
        /// `None` (the default; skipped when serializing, so finalized entries and old
        /// logs are byte-identical) = a finalized turn, projected as always. A record
        /// with no diagnostics serializes as a bare string, the shape older logs
        /// wrote, and either shape deserializes.
        ///
        /// [`project`]: crate::project::project
        #[serde(default, skip_serializing_if = "Option::is_none")]
        upstream_failure: Option<UpstreamFailureRecord>,
    },
    /// The result of running one tool call. `raw` is immutable; curation only
    /// flips `wire`.
    ToolResult {
        call_id: CallId,
        raw: RawResult,
        wire: WireState,
    },
    /// The final wire decision for a previously captured pending result.
    /// Raw output stays on the referenced entry (docs/triage.md).
    ResultVerdict { for_result: Seq, wire: WireState },
    /// A fact about how the session ran rather than what it said (the
    /// [`Telemetry`] family): projects to nothing, and every reader that renders a
    /// session skips it (docs/log-and-projection.md).
    Telemetry(Telemetry),
    /// The session was reopened from its log by a later host process
    /// (docs/log-and-projection.md). Projects the restart notice at its position: how
    /// long the session sat, which background processes it no longer monitors, and
    /// which calls were dropped because they started and never returned.
    Resumed(Resumed),
    /// Text that replaces a range of the transcript, made by the agent's
    /// `consolidate` call or by the harness's compaction (docs/summary.md).
    /// Projects the text where the range ends and elides the range
    /// (docs/log-and-projection.md).
    Summary(Summary),
    /// A triage verdict (docs/triage.md) — the durable record of a throwaway
    /// triage's decision. The fork's *transcript* stays throwaway; its *decision* is
    /// logged here for audit / introspection / triager-quality work. Projects as
    /// audit only, except it emits a block per **pinned** notification (its `inbox`)
    /// at this entry's tail-ward position (docs/inbox.md).
    Triage(Triage),
    /// An out-of-band notification (docs/inbox.md), logged at **first-show** — the
    /// moment a triage first shows it to the model. Pure audit on the canonical
    /// wire (it projects to nothing); a *pinned* one is emitted by its resolving
    /// [`Triage`], at that entry's position. Its `seq` is its durable id (the handle
    /// the `retain` inbox verdict references).
    Ambient(Notification),
    /// A spotter verdict (docs/permissions.md) — the durable record of the
    /// independent permission rung's decision on one gated tool call. Like
    /// [`Triage`], the deciding round-trip is throwaway but its *verdict* is logged:
    /// the audit answer to "why did this call get through (or stop)?" and the corpus
    /// for any future triaged approval model. Projects to nothing — it is audit
    /// only, never on the canonical wire. Fast-path clears are deliberately *not*
    /// logged; the spotter is the first permission rung that earns an entry.
    Spot(Spot),
    /// One lifecycle fact about a background process — its mint or its exit
    /// (docs/process-monitor.md). The process monitor's registry is ephemeral, but a
    /// `p#<n>` handle appears in prose (the backgrounding result, the exit
    /// notification) that outlives it, so these entries are what a reader of a
    /// recorded session — or a restarted daemon — resolves the handle against.
    /// Metadata only: the command, the spawning call, the spawn time, the exit
    /// status and duration. A process's *output* is the registry's bounded
    /// best-effort capture and is deliberately not recorded here.
    ///
    /// Projects to nothing, like [`Telemetry`] — the exit still reaches the
    /// model as an [`Ambient`] notification, and this is the audit record beside
    /// it, not a second copy on the wire (docs/log-and-projection.md).
    ///
    /// [`Telemetry`]: Payload::Telemetry
    /// [`Ambient`]: Payload::Ambient
    ProcessEvent(ProcessEvent),
    /// A config edit — the durable record of one change to the policy table
    /// (docs/permissions.md). The config-edit loop turns a deny into
    /// negotiation: an orchestrator (human or coordinating session) edits the session's
    /// config, and the change lands here. Effective policy = base config + the fold of these
    /// (see [`effective_config`]); the log is the source of truth for what changed, exactly as
    /// it is for everything else (docs/log-and-projection.md). The fold is **replace-at-key**
    /// (last-write-wins, order-defined) — materializing a table from an ordered log, like a
    /// database replaying its write-ahead log. Audit-only on the wire — it projects to nothing,
    /// like [`Spot`]; its effect reaches the model *by key*: a `commands`/`tools` edit only
    /// through the classifier, while a **session-visible** edit (`scope`, `paths`) *also*
    /// surfaces as an [`Ambient`] notification (an optional tail append — never a re-render of
    /// the cached preamble; see [`crate::daemon`]) so the session learns its perimeter moved.
    ///
    /// [`effective_config`]: crate::policy::effective_config
    /// [`Ambient`]: Payload::Ambient
    ConfigEdit(ConfigEdit),
    /// The boot preamble snapshot (docs/context.md): the full rendered `system`
    /// string, exactly as this session's requests carry it, appended **once at boot**. The
    /// preamble folds in `[context]` file contents that drift on disk between boots, so it is
    /// not reconstructable from `config + current files` — this entry makes the log the
    /// complete record of the wire (docs/log-and-projection.md). Audit/replay only: it projects to
    /// nothing (the system prompt rides the request's `system` field, never the `messages`
    /// array); the `wire` dev view reads it back instead of re-resolving files from disk.
    ///
    /// `context` carries the **structured** `[context]` listing (name / `path` / description
    /// per item) the flat `system` string was rendered from, snapshotted **additively** so the
    /// listing survives as a fold over the log — the source `project` re-renders a surfaced
    /// context item from (docs/triage.md), the same way it looks up pinned
    /// notification bodies. `system` stays the byte-exact wire truth; `context` never touches
    /// the wire. `#[serde(default)]`: logs predating the field read back an empty listing.
    Preamble {
        system: String,
        #[serde(default)]
        context: crate::context::Resolved,
        /// The fixed main inference identity chosen when this session booted:
        /// connection, provider, and provider-native model id. It is log metadata
        /// only — an offline reader uses it to derive the historical dialect and
        /// projection placements, while `project` emits nothing for it
        /// (docs/log-and-projection.md). `None` on pre-identity logs.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        inference: Option<LoggedInference>,
    },
    /// The session's title — one line saying what the session is for
    /// (docs/session-title.md). Written by the title peek after a trajectory ends,
    /// or by an operator over the control channel; the latest entry is the current
    /// title, which the `session_identity` topic carries. Appended even when the
    /// triage kept the previous text, so the log records that the question was asked.
    /// Log metadata: it projects to nothing, like [`Telemetry`], and never
    /// touches the wire or the cached prefix (docs/log-and-projection.md).
    ///
    /// [`Telemetry`]: Payload::Telemetry
    Title(Title),
    /// The session's own git worktree was created at boot: path, branch, base commit, and
    /// git's output and exit status, which carry the project's `post-checkout` hook's
    /// result (docs/control-channel.md). Projects to nothing, like [`Telemetry`]; appended
    /// right after the boot [`Preamble`].
    ///
    /// [`Preamble`]: Payload::Preamble
    Worktree(WorktreeEvent),
    /// The endpoint refused a request carrying images, and the same request with
    /// them withheld succeeded (docs/transport.md). From here the session's requests
    /// carry a placeholder in each image's place and the tool layer refuses new
    /// images; a resumed session reads this entry back to restore both. `refusal`
    /// is the provider's error. Projects to nothing, like [`Telemetry`]: the placeholder
    /// is the client's encoding, not this entry's projection.
    ///
    /// [`Telemetry`]: Payload::Telemetry
    ImagesRefused { refusal: String },
}

/// A session reopened from its log (docs/log-and-projection.md): what the loader
/// knows changed across the restart, and what it set aside to continue.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Resumed {
    /// Milliseconds between the last entry before the restart and this one.
    pub gap_ms: u64,
    /// Background handles (`p#<n>`) that were spawned with no recorded exit: their
    /// output is no longer captured, and whether they still run is unknown.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub lost_processes: Vec<String>,
    /// Present only when the log ended uncleanly: the seq of the last entry this session
    /// builds on. Entries after it and before this one stay on disk but are not
    /// restored, and projection skips them (docs/resumption.md). A log written before the
    /// field existed reads as `None`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resumes_after: Option<Seq>,
    /// Present only when the session resumed at an unfinished batch rather than past it:
    /// the calls of the last live completion that have not run (or whose results are
    /// still undecided), which the operator's next action brings back. The notice
    /// projects after the batch's results land, not at this entry's position
    /// (docs/resumption.md). A log written before the field existed reads as empty.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pending_calls: Vec<CallId>,
}

impl Resumed {
    /// The entries a `Resumed` entry at `at` skips: every seq after `resumes_after`
    /// and before `at`. Empty when nothing was skipped.
    pub fn skipped(&self, at: Seq) -> std::ops::Range<Seq> {
        match self.resumes_after {
            Some(after) if after + 1 < at => after + 1..at,
            _ => 0..0,
        }
    }
}

/// One title decision (docs/session-title.md): the text and who wrote it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Title {
    pub text: String,
    pub author: TitleAuthor,
}

/// Who set a [`Title`]: the triage model's title ask, or an operator over the control
/// channel (docs/session-title.md).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TitleAuthor {
    Triage,
    Operator,
}

/// The inference identity persisted in a boot [`Payload::Preamble`].
///
/// This is deliberately a log-owned shape rather than the control/configuration
/// selection type. Boot copies the three facts offline readers need into this
/// record, so a rename in the live configuration model does not change how an
/// existing log deserializes (docs/log-and-projection.md).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LoggedInference {
    pub connection: String,
    pub provider: crate::api::Provider,
    pub model: String,
}

impl LoggedInference {
    /// Construct the persisted identity from the three values recorded at boot.
    pub fn new(
        connection: impl Into<String>,
        provider: crate::api::Provider,
        model: impl Into<String>,
    ) -> Self {
        Self {
            connection: connection.into(),
            provider,
            model: model.into(),
        }
    }
}

/// The current title: the text of the last [`Payload::Title`] in `entries`, or
/// `None` before any was written (docs/session-title.md).
pub fn latest_title(entries: &[LogEntry]) -> Option<&str> {
    entries.iter().rev().find_map(|e| match &e.payload {
        Payload::Title(t) => Some(t.text.as_str()),
        _ => None,
    })
}

/// The inference identity recorded by the latest boot [`Payload::Preamble`], or
/// `None` for a log written before that metadata existed. The latest preamble is
/// the boot record for a multi-boot log; an older identity is not resurrected when
/// the latest preamble has none (docs/log-and-projection.md).
pub fn recorded_inference(entries: &[LogEntry]) -> Option<&LoggedInference> {
    entries
        .iter()
        .rev()
        .find_map(|entry| match &entry.payload {
            Payload::Preamble { inference, .. } => Some(inference.as_ref()),
            _ => None,
        })
        .flatten()
}

impl Payload {
    /// A tool result payload for `call_id`. The one construction site in production
    /// and in tests, so a field added to the result shape lands in one place.
    pub fn tool_result(call_id: impl Into<CallId>, raw: RawResult, wire: WireState) -> Self {
        Self::ToolResult {
            call_id: call_id.into(),
            raw,
            wire,
        }
    }
}

/// A content block authored by the model or user. `Thinking` / `RedactedThinking`
/// carry the extended-thinking payload (docs/transport.md): `signature` is the
/// opaque per-block token the API mints and requires echoed **verbatim** on replay
/// (it rejects a modified block); `redacted_thinking` carries opaque `data` instead
/// of text. Images are not blocks here — a tool result's images live on
/// [`RawResult`], where the bytes are stored as they go on the wire.
///
/// `Attachment` is a file the operator's composer attached to a `User` turn. It
/// records the file's name, path, and triage signals, never its bytes: it
/// projects as the same notice an ambient arrival carries, and the model pulls
/// the file with `read` / `read_image` if it wants it (docs/inbox.md).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ContentBlock {
    Text(String),
    Thinking { text: String, signature: String },
    RedactedThinking { data: String },
    Attachment(crate::attachments::Arrival),
}

/// A tool invocation emitted by the model. `input` is a [`serde_json::Value`],
/// whose object maps serialize with sorted keys — deterministic, sidestepping
/// the `HashMap`-key-order cache hazard (docs/log-and-projection.md).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: CallId,
    pub name: String,
    pub input: Value,
}

impl ToolCall {
    /// A call by tool name, call id, and input. The one way to construct a call,
    /// in production and in tests alike, so a field added later lands in one place.
    pub fn new(name: impl Into<String>, id: impl Into<CallId>, input: Value) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            input,
        }
    }

    /// Set the harness-injected `intent` inside `input`, where the wire carries it
    /// (docs/tools.md). Builds; it does not amend a recorded call.
    pub fn with_intent(mut self, intent: impl Into<String>) -> Self {
        if let Value::Object(map) = &mut self.input {
            map.insert(crate::tools::INTENT.into(), Value::String(intent.into()));
        }
        self
    }

    /// The harness-injected `intent` (docs/tools.md): why this call was made, in the
    /// model's words, written before it ran. `None` for a call from before the field
    /// existed, one the model omitted, or a tool that declares its own `intent` — no
    /// reader may assume it is present.
    ///
    /// It stays in `input` rather than becoming a field of its own: the wire carries
    /// it as part of the `tool_use` block, so it is already in the projection and in
    /// every recorded log, and lifting it out would move the pinned wire bytes
    /// (docs/log-and-projection.md). Only the dispatch path removes it.
    pub fn intent(&self) -> Option<&str> {
        self.input
            .get(crate::tools::INTENT)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|intent| !intent.is_empty())
    }
}

/// What the agent chose to do with one notification at a triage (docs/inbox.md).
/// "Leave" (bump to the next triage) is the *absence* of an entry, so it isn't a variant.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Disposition {
    /// Promote into canonical working context (projects at the [`Triage`] position).
    Pin,
    /// Clear from the inbox (logged for audit, never reaches the wire).
    Dismiss,
}

/// An out-of-band notification (docs/inbox.md): freeform `body` + `source`, optionally
/// bound to a logical `slot` (for supersession in the daemon's pending queue).
/// Immutable; once logged as a [`Payload::Ambient`] its arrival `seq` is its id.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Notification {
    pub source: String,
    pub body: String,
    pub slot: Option<String>,
}
