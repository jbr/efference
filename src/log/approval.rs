//! The decision a gate returns, as the log records it (docs/permissions.md,
//! docs/log-and-projection.md).

use serde::{Deserialize, Serialize};

use super::ConfigChange;

/// The decision returned by
/// [`Frontend::approve`](crate::frontend::Frontend::approve) (docs/README.md). A typed
/// line is a *redirect* — the call is rejected and the human's text is fed to the model
/// as the next user turn, steering it without ending the turn. Every un-run
/// `tool_use` still gets a paired `is_error` `tool_result` so the wire never
/// orphans a call (the API rejects that); the loop owns that pairing.
///
/// Serialized into the log as a gate decision's record (`Telemetry::GateDecision`,
/// docs/log-and-projection.md), so a rename here changes what new entries say.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
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
