use super::{Control, Executor};
use anyhow::{Result, anyhow};
use mcplease::{
    traits::{Tool, ToolMeta, ToolOutput as _},
    types::{RequestContext, ToolAnnotations},
};
use serde::{Deserialize, Serialize};

/// Send a user message to the session, as its next turn. Seeds turn 1 on an idle
/// session; on a busy one it lands as soon as the current turn finishes. Returns as
/// soon as the message is accepted — set `wait` to block until the session reaches
/// its next gate or finishes the turn, and read the resulting state instead.
///
/// This is the interrupting channel: it becomes a user message the model answers.
/// For something that can wait for a natural break, use `notify`.
#[derive(Debug, Serialize, Deserialize, schemars::JsonSchema, clap::Args)]
#[serde(rename = "turn")]
pub struct Turn {
    /// The turn text. Fragments are joined with spaces, so a single-element array is
    /// the ordinary call.
    #[arg(required = true, num_args = 1..)]
    pub text: Vec<String>,
    /// Block until the session settles — the turn reaches a gate or completes — and
    /// answer with its state rather than with an acknowledgement.
    #[serde(default)]
    #[arg(long)]
    pub wait: bool,
    /// Seconds to wait before giving up on `wait`. On expiry the current state comes
    /// back marked `settled: false`, which is not an error.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[arg(long)]
    pub timeout_s: Option<u64>,
    /// Name who is submitting, when it is not the operator (docs/control-api.md):
    /// `web`, `mcp[<name>]`, or `agent[<id>]`. Over MCP this defaults to
    /// `mcp[<caller's clientInfo.name>]`, so a mounting harness is named without
    /// saying anything; a script passes it explicitly. Absent = the operator.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[arg(long)]
    pub source: Option<String>,
    /// Drive this session by id (see `sessions`). Required unless the server was started with `--name`/`--socket`; then it overrides that session for this call.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[arg(long)]
    pub session: Option<String>,
}

impl ToolMeta for Turn {
    fn title() -> Option<&'static str> {
        Some("Send a turn")
    }

    fn annotations() -> Option<ToolAnnotations> {
        Some(ToolAnnotations {
            read_only_hint: Some(false),
            destructive_hint: Some(false),
            // Two identical turns are two turns.
            idempotent_hint: Some(false),
            open_world_hint: Some(false),
            ..Default::default()
        })
    }
}

impl Tool<Control> for Turn {
    type Output = String;

    fn execute(self, control: &mut Control, context: &RequestContext) -> Result<String> {
        match &mut control.executor {
            Executor::Client(state) => {
                let client = state.client(self.session.as_deref())?;
                let text = self.text.join(" ");
                // An explicit `--source` wins; otherwise a call over MCP names the
                // mounting harness from the clientInfo it sent (docs/control-api.md). The
                // command line's RequestContext is empty, so `efference control turn`
                // submits as the operator unless told otherwise.
                let source = turn_source(self.source, context);
                if !self.wait {
                    state.block_on(client.turn_sourced(&text, source.as_deref()))?;
                    return Ok("accepted".into());
                }
                let settled = state.settle_within(
                    &client,
                    self.timeout_s,
                    client.turn_wait_sourced(&text, source.as_deref()),
                )?;
                Ok(settled.to_text())
            }
            Executor::Host(state) => {
                // In-session there is nothing to block on: the calling session
                // has its own inbox (docs/tools.md).
                if self.wait {
                    return Err(anyhow!(
                        "`wait` is not available in-session: a session's continuation is its own \
                         inbox, not a blocking call (docs/tools.md)"
                    ));
                }
                let text = self.text.join(" ");
                // A turn a session submits is attributed to it (docs/tools.md),
                // beside the `mcp[<name>]` shape an external mount uses.
                let source = format!("session[{}]", state.caller);
                state
                    .resolve_mutating(self.session.as_deref())?
                    .submit_turn(crate::frontend::SubmittedTurn {
                        text,
                        submission_id: crate::daemon::mint_submission_id(),
                        source: Some(source),
                    });
                Ok("accepted".into())
            }
        }
    }
}

/// The `source` a submitted turn carries (docs/control-api.md): an explicit
/// label wins; otherwise a call over MCP names its caller from the `clientInfo`
/// the caller sent, so a mounting harness is named without saying anything.
/// `None` (the command line's empty context, or a caller that declared no name)
/// means the operator, whose turns render no prefix.
fn turn_source(explicit: Option<String>, context: &RequestContext) -> Option<String> {
    explicit.or_else(|| {
        context
            .client_info
            .as_ref()
            .map(|info| format!("mcp[{}]", info.name))
    })
}

#[cfg(test)]
mod test;
