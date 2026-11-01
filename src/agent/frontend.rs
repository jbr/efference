//! The `Frontend` — the user-facing interaction boundary (docs/README.md).
//!
//! The agent loop knows only this trait; whichever backend is mounted sits behind
//! it (docs/control-channel.md). The one mounted backend is
//! [`crate::agent::remote_frontend::RemoteFrontend`], fed by the daemon's HTTP handlers:
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

use crate::record::log::{Approval, ToolCall};
use crate::session::approvals::GateOptions;
use crate::session::submitted::SubmittedTurn;
use crate::tools::Interrupt;

/// The interaction boundary. Used generically by the loop (`<F: Frontend>`); not
/// object-safe, and it needn't be — there's one mounted backend at a time. The
/// futures are `Send` so the loop can run on the host's multi-thread runtime
/// (docs/control-channel.md); implementors write `async fn` as usual.
pub trait Frontend {
    /// Read the next user turn. `None` = end of input → quit.
    fn read_user_turn(&mut self) -> impl Future<Output = Option<SubmittedTurn>> + Send;

    /// Non-blocking drain of any user turns submitted **while the loop was
    /// mid-trajectory** (docs/control-channel.md): the "next possible juncture"
    /// delivery that keeps a mid-trajectory message from waiting until the whole
    /// trajectory ends. Returns the queued turns in submission order (blank
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
