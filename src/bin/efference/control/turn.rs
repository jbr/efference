use super::Control;
use anyhow::Result;
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

    fn execute(self, state: &mut Control, _context: &RequestContext) -> Result<String> {
        let client = state.client(self.session.as_deref())?;
        let text = self.text.join(" ");
        if !self.wait {
            state.block_on(client.turn(&text))?;
            return Ok("accepted".into());
        }
        let settled = state.settle_within(&client, self.timeout_s, client.turn_wait(&text))?;
        Ok(settled.to_text())
    }
}
