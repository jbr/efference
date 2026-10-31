use super::ClientExecutor;
use super::args::AgentOrHere;
use super::verb::{ControlVerb, VerbCall};
use anyhow::Result;
use mcplease::{traits::ToolMeta, types::ToolAnnotations};
use serde::{Deserialize, Serialize};

/// Set the session's title: the one line the session list, the TUI sidebar, and
/// the web top bar show for it. The triage model keeps a title current on its
/// own; this overrides it, and the next model-written title starts from what you
/// set. The session's id (its address) never changes.
#[derive(Debug, Serialize, Deserialize, schemars::JsonSchema, clap::Args)]
#[serde(rename = "title")]
pub struct Title {
    /// The title, one line.
    pub title: String,
    #[serde(default, skip_serializing_if = "AgentOrHere::is_none")]
    #[command(flatten)]
    pub agent: AgentOrHere,
}

impl ToolMeta for Title {
    fn title() -> Option<&'static str> {
        Some("Set the session title")
    }

    fn annotations() -> Option<ToolAnnotations> {
        Some(ToolAnnotations {
            read_only_hint: Some(false),
            destructive_hint: Some(false),
            idempotent_hint: Some(true),
            open_world_hint: Some(false),
            ..Default::default()
        })
    }
}

impl ControlVerb for Title {
    type Output = String;

    async fn client(self, state: &ClientExecutor, _call: &VerbCall<'_>) -> Result<String> {
        let client = state.client(self.agent.id()).await?;
        let identity = client.set_title(&self.title).await?;
        Ok(format!(
            "{}: {}",
            identity["id"].as_str().unwrap_or("session"),
            identity["title"].as_str().unwrap_or_default()
        ))
    }
}
