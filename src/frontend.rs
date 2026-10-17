//! The `Frontend` — the user-facing interaction boundary (docs/README.md).
//!
//! The agent loop knows only this trait; whichever backend is mounted sits behind
//! it. The control channel makes HTTP the *singular, authoritative* input source
//! (docs/control-channel.md), so the one mounted backend is
//! [`crate::daemon::RemoteFrontend`], fed by the daemon's HTTP handlers. There is
//! no privileged in-process path — the human (via the TUI), an automated driver,
//! and a subagent are all clients of the same socket.
//!
//! ## Three interaction points
//!
//! - [`Frontend::read_user_turn`] — the next user turn, and the first one too, since
//!   turns arrive over the socket rather than argv. `None` ends the loop.
//! - [`Frontend::approve`] — a gate on a pending tool call. Approve runs it; a
//!   redirect rejects it and steers the model with the supplied text; abort
//!   unwinds the turn.
//! - [`Frontend::interrupted`] — resolves when the user interrupts a *running*
//!   tool, carrying the intent: kill the process group, or detach it into the
//!   background process monitor (docs/process-monitor.md).

use crate::control_tools::BoundAction;
use crate::log::{ConfigChange, ContentBlock, ToolCall};

/// The `source` of the kickoff turn a `first-turn` config submits (docs/context.md).
/// Reserved: the clients read it to leave the turn out of the transcript, since it
/// belongs to the session's opening the way the preamble does, not to the operator.
pub const KICKOFF_SOURCE: &str = "efference";

/// The `source` of a hand-off brief: the predecessor's id in the reserved
/// `handoff[…]` form (docs/tools.md). The clients render it as the brief it is
/// rather than as the operator's turn, which is why a client may not claim it.
pub fn handoff_source(predecessor: &str) -> String {
    format!("handoff[{predecessor}]")
}

/// The predecessor a hand-off brief's `source` names, or `None` for any other.
pub fn handoff_predecessor(source: &str) -> Option<&str> {
    source.strip_prefix("handoff[")?.strip_suffix(']')
}

/// The `source` of a turn or decision another session sends: `session[<id>]`
/// (docs/control-api.md).
pub fn session_source(id: &str) -> String {
    format!("session[{id}]")
}

/// The session a `session[<id>]` source names, or `None` for any other.
pub fn source_session(source: &str) -> Option<&str> {
    source.strip_prefix("session[")?.strip_suffix(']')
}

/// How a turn's `source` opens it on the wire (docs/log-and-projection.md): a turn
/// from another session as `<id> (agent)`, any other label as written.
pub fn wire_label(source: &str) -> std::borrow::Cow<'_, str> {
    match source_session(source) {
        Some(id) => format!("{id} (agent)").into(),
        None => source.into(),
    }
}

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
    pub attachments: Vec<crate::attachments::Arrival>,
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

    pub fn with_attachments(mut self, attachments: Vec<crate::attachments::Arrival>) -> Self {
        self.attachments = attachments;
        self
    }

    /// Nothing to deliver: blank text and no attachments. A blank submit keeps the
    /// loop idling rather than landing an empty `User` entry on the wire.
    pub fn is_blank(&self) -> bool {
        self.text.trim().is_empty() && self.attachments.is_empty()
    }

    /// A pending turn as the `queued` topic and a recall present it
    /// (docs/control-api.md): `attachments` only when there are any, so a
    /// text-only turn reads as it always has.
    pub fn pending_json(&self) -> serde_json::Value {
        let mut json = serde_json::json!({
            "submission_id": self.submission_id,
            "text": self.text,
        });
        if !self.attachments.is_empty() {
            json["attachments"] = serde_json::to_value(&self.attachments).unwrap_or_default();
        }
        json
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
            .chain(self.attachments.iter().map(crate::attachments::turn_line))
            .collect::<Vec<_>>()
            .join("\n")
    }
}

/// The operator's intent when interrupting a *running* tool (docs/process-monitor.md).
/// Both reach the one intervention point pace can't — mid-tool — but differ in
/// what happens to the child.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Interrupt {
    /// Kill the process group now (`POST /loop/interrupt`, the Ctrl-C guardrail).
    Kill,
    /// Keep the child running but stop blocking on it: hand it to the background
    /// process monitor as a `p#<n>` process (`PATCH /processes/:id {background: true}`, the
    /// foreground→background handoff, docs/process-monitor.md).
    Detach,
}

/// The decision returned by [`Frontend::approve`] (docs/README.md). A typed line is a
/// *redirect* — the call is rejected and the human's text is fed to the model as
/// the next user turn, steering it without ending the turn. Every un-run
/// `tool_use` still gets a paired `is_error` `tool_result` so the wire never
/// orphans a call (the API rejects that); the loop owns that pairing.
///
/// Serialized into the log as a gate decision's record (`Telemetry::GateDecision`,
/// docs/log-and-projection.md), so a rename here changes what new entries say.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Approval {
    /// Run the call.
    Approve,
    /// Don't run it; bail the turn.
    Abort,
    /// Don't run it; reject the call and send this text to the model as the next
    /// user message, then keep going. The human's words land as a real user turn
    /// (after the rejection `tool_result`), so the model reads it as direction,
    /// not tool output.
    Redirect(String),
    /// **Amend policy**, then run this call iff the amended policy now clears it
    /// (docs/permissions.md). The orchestrator's reply at a gate: extend
    /// the config with a [`ConfigChange`], written to the session's config file and logged as a
    /// `Payload::ConfigEdit`. The current call does not "ride along" — the loop re-classifies it
    /// against the new policy and only runs it if it clears, so the mechanism (not the
    /// human's gesture) authorizes the run. `note` is the rationale, for the audit corpus.
    ConfigEdit { change: ConfigChange, note: String },
}

/// The interaction boundary. Used generically by the loop (`<F: Frontend>`); not
/// object-safe, and it needn't be — there's one mounted backend at a time. The
/// futures are `Send` so the loop can run on the host's multi-thread runtime
/// (docs/control-channel.md); implementors write `async fn` as usual.
pub trait Frontend {
    /// Read the next user turn. `None` = end of input → quit.
    fn read_user_turn(&mut self) -> impl Future<Output = Option<SubmittedTurn>> + Send;

    /// Non-blocking drain of any user turns submitted **while the loop was
    /// mid-trajectory** (docs/control-channel.md). The loop calls this at
    /// each inner-loop juncture (after a tool's results land, before the next model
    /// call) and appends each as a canonical `Payload::User` — the "next possible
    /// juncture" delivery that keeps a mid-trajectory message from waiting until the
    /// whole trajectory ends. Returns the queued turns in submission order (blank
    /// turns filtered); empty when nothing is queued. Default: none — a frontend with
    /// no out-of-band turn queue never has anything to drain.
    fn drain_queued_turns(&mut self) -> Vec<SubmittedTurn> {
        Vec::new()
    }

    /// Gate a pending tool call: approve, abort, or redirect. `bound` is the leaf
    /// action a `agents__decide` authorizes, when the call is one
    /// (docs/permissions.md) — the gate view and the child's own held action, so
    /// the operator sees the move rather than an id.
    fn approve(
        &mut self,
        call: &ToolCall,
        bound: Option<&BoundAction>,
    ) -> impl Future<Output = Approval> + Send;

    /// Resolve when the user interrupts a *running* tool, with the intent: kill
    /// its process group, or detach it into the background process monitor. The loop
    /// passes this as the bash tool's `abort` future (docs/process-monitor.md).
    fn interrupted(&mut self) -> impl Future<Output = Interrupt> + Send;
}
