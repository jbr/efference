//! The `background` session-control verb (docs/tools.md).

use super::ClientExecutor;
use super::args::AgentOrHere;
use super::verb::{ControlVerb, VerbCall};
use anyhow::Result;
use mcplease::{traits::ToolMeta, types::ToolAnnotations};
use serde::{Deserialize, Serialize};

/// Detach the tool the session is running into the background process monitor: it keeps
/// running as a `p#<n>` process and stops blocking the turn. The alternative to
/// `interrupt` for a command that should finish but should not be waited on. No-op
/// when nothing is mid-tool, and it lands only while the call is still running.
#[derive(Debug, Serialize, Deserialize, schemars::JsonSchema, clap::Args)]
#[serde(rename = "background")]
pub struct Background {
    /// A message delivered to the model as the next user turn, alongside the handoff
    /// — what it should do while the process runs.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[arg(long)]
    pub note: Option<String>,
    #[serde(default, skip_serializing_if = "AgentOrHere::is_none")]
    #[command(flatten)]
    pub agent: AgentOrHere,
}

impl ToolMeta for Background {
    fn title() -> Option<&'static str> {
        Some("Background the running tool")
    }

    fn annotations() -> Option<ToolAnnotations> {
        Some(ToolAnnotations {
            read_only_hint: Some(false),
            destructive_hint: Some(false),
            idempotent_hint: Some(false),
            open_world_hint: Some(false),
            ..Default::default()
        })
    }
}

impl ControlVerb for Background {
    type Output = String;

    async fn client(self, state: &ClientExecutor, _call: &VerbCall<'_>) -> Result<String> {
        let client = state.client(self.agent.id()).await?;
        Ok(match client.background(self.note.as_deref()).await? {
            Some(id) => {
                format!("requested backgrounding p#{id} (lands only while it is still running)")
            }
            None => "no foreground tool running; nothing to background".into(),
        })
    }
}
