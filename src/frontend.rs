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

use crate::protocol::agents::BoundAction;
use crate::protocol::turns::QueuedTurn;
use crate::record::log::{Approval, ContentBlock, ToolCall};
use crate::tools::Interrupt;

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

/// What a gate carries beyond the held call (docs/permissions.md): the optional inputs of
/// [`Frontend::approve`], which the session registers on the gate it publishes. The
/// default is a plain gate, with no bound action and no grant on offer.
#[derive(Clone, Debug, Default)]
pub struct GateOptions<'a> {
    /// The leaf action an `agents__decide` authorizes, when the held call is one — the
    /// gate view and the child's own held action, so the operator sees the move rather
    /// than an id.
    pub bound: Option<&'a BoundAction>,
    /// The one config change the gate offers as "always allow", decided by the rung that
    /// held the call.
    pub grant: Option<crate::policy::GateGrant>,
}

impl<'a> GateOptions<'a> {
    /// This gate, for the leaf action `bound` authorizes.
    pub fn with_bound(mut self, bound: Option<&'a BoundAction>) -> Self {
        self.bound = bound;
        self
    }

    /// This gate, offering `grant`.
    pub fn with_grant(mut self, grant: Option<crate::policy::GateGrant>) -> Self {
        self.grant = grant;
        self
    }
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

    /// Gate a pending tool call: approve, abort, or redirect. `gate` carries what the
    /// gate shows beside the call: the leaf action an `agents__decide` authorizes and
    /// the grant on offer (docs/permissions.md).
    fn approve(
        &mut self,
        call: &ToolCall,
        gate: GateOptions<'_>,
    ) -> impl Future<Output = Approval> + Send;

    /// Resolve when the user interrupts a *running* tool, with the intent: kill
    /// its process group, or detach it into the background process monitor. The loop
    /// passes this as the bash tool's `abort` future (docs/process-monitor.md).
    fn interrupted(&mut self) -> impl Future<Output = Interrupt> + Send;
}
