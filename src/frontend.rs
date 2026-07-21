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
//! - [`Frontend::read_user_turn`] — the next user turn (the first one too, now
//!   that turns arrive over the socket rather than argv). `None` ends the loop.
//! - [`Frontend::approve`] — a gate on a pending tool call. Approve runs it; a
//!   redirect rejects it and steers the model with the supplied text; abort
//!   unwinds the turn.
//! - [`Frontend::interrupted`] — resolves when the user interrupts a *running*
//!   tool, so the loop can kill the process group (docs/supervisor.md).

use crate::log::{ConfigChange, Lifetime, ToolCall};

/// The decision returned by [`Frontend::approve`] (docs/README.md). A typed line is a
/// *redirect* — the call is rejected and the human's text is fed to the model as
/// the next user turn, steering it without ending the turn. Every un-run
/// `tool_use` still gets a paired `is_error` `tool_result` so the wire never
/// orphans a call (the API rejects that); the loop owns that pairing.
#[derive(Clone, Debug, PartialEq, Eq)]
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
    /// (backlog/permissions/config-edit.md). The orchestrator's reply at a gate: extend
    /// the config with a [`ConfigChange`] (session-only or persisted), logged as a
    /// `Payload::ConfigEdit`. The current call does not "ride along" — the loop re-classifies it
    /// against the new policy and only runs it if it clears, so the mechanism (not the
    /// human's gesture) authorizes the run. `note` is the rationale, for the audit corpus.
    ConfigEdit {
        change: ConfigChange,
        lifetime: Lifetime,
        note: String,
    },
}

/// The interaction boundary. Used generically by the loop (`<F: Frontend>`); not
/// object-safe, and it needn't be — there's one mounted backend at a time.
#[allow(async_fn_in_trait)] // single-consumer interaction; no Send bound needed.
pub trait Frontend {
    /// Read the next user turn. `None` = end of input → quit.
    async fn read_user_turn(&mut self) -> Option<String>;

    /// Non-blocking drain of any user turns submitted **while the loop was
    /// mid-trajectory** (docs/control-channel.md). The loop calls this at
    /// each inner-loop juncture (after a tool's results land, before the next model
    /// call) and appends each as a canonical `Payload::User` — the "next possible
    /// juncture" delivery that keeps a mid-trajectory message from waiting until the
    /// whole trajectory ends. Returns the queued turns in submission order (blank
    /// turns filtered); empty when nothing is queued. Default: none — a frontend with
    /// no out-of-band turn queue never has anything to drain.
    fn drain_injected_turns(&mut self) -> Vec<String> {
        Vec::new()
    }

    /// Gate a pending tool call: approve, abort, or redirect.
    async fn approve(&mut self, call: &ToolCall) -> Approval;

    /// Resolve when the user interrupts a *running* tool. The loop passes this as
    /// the bash tool's `abort` future so the process group is killed (docs/supervisor.md).
    async fn interrupted(&mut self);
}
