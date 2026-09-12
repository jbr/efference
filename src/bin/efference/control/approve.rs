use super::Control;
use anyhow::Result;
use efference::frontend::Approval;
use mcplease::{
    traits::{Tool, ToolMeta, ToolOutput as _},
    types::{RequestContext, ToolAnnotations},
};
use serde::{Deserialize, Serialize};

/// Resolve the gate the session is stopped at. With neither `abort` nor `redirect`,
/// the held tool call is approved and the loop continues.
///
/// The call id defaults to the one pending gate, which is what `state` reports; a
/// session runs one call at a time, so naming it is only needed to be sure the gate
/// has not moved since you read it.
#[derive(Debug, Serialize, Deserialize, schemars::JsonSchema, clap::Args)]
#[serde(rename = "approve")]
pub struct Approve {
    /// The held call's `tool_use` id. Defaults to whatever gate is pending.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub call_id: Option<String>,
    /// Reject the call and unwind the whole turn, rather than approving it.
    #[serde(default)]
    #[arg(long, conflicts_with = "redirect")]
    pub abort: bool,
    /// Reject this one call and steer the model with this message, which is kept as
    /// the next user turn. The turn continues.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[arg(long)]
    pub redirect: Option<String>,
    /// Block until the session settles again — its next gate, or the turn completing
    /// — and answer with its state. This is how a multi-gate turn is driven as a
    /// clean approve-then-read loop.
    #[serde(default)]
    #[arg(long)]
    pub wait: bool,
    /// Seconds to wait before giving up on `wait`. On expiry the current state comes
    /// back marked `settled: false`, which is not an error.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[arg(long)]
    pub timeout_s: Option<u64>,
    /// Drive this session by id (see `sessions`). Required unless the server was started with `--name`/`--socket`; then it overrides that session for this call.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[arg(long)]
    pub session: Option<String>,
}

impl ToolMeta for Approve {
    fn title() -> Option<&'static str> {
        Some("Resolve a gate")
    }

    fn annotations() -> Option<ToolAnnotations> {
        Some(ToolAnnotations {
            read_only_hint: Some(false),
            // Approving runs a call the rungs did not clear; what it then does is
            // the call's business, not this tool's.
            destructive_hint: Some(true),
            idempotent_hint: Some(false),
            open_world_hint: Some(false),
            ..Default::default()
        })
    }
}

impl Tool<Control> for Approve {
    type Output = String;

    fn execute(self, state: &mut Control, _context: &RequestContext) -> Result<String> {
        let client = state.client(self.session.as_deref())?;
        let call_id = match self.call_id {
            Some(id) => id,
            None => state.block_on(super::pending_call_id(&client))?,
        };
        let decision = match (self.abort, self.redirect) {
            (true, _) => Approval::Abort,
            (false, Some(message)) => Approval::Redirect(message),
            (false, None) => Approval::Approve,
        };
        if !self.wait {
            state.block_on(client.approve(&call_id, &decision))?;
            return Ok(format!("resolved {call_id}"));
        }
        let settled = state.settle_within(
            &client,
            self.timeout_s,
            client.approve_wait(&call_id, &decision),
        )?;
        Ok(settled.to_text())
    }
}
