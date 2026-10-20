use super::{Control, Executor};
use anyhow::Result;
use mcplease::{
    traits::{Tool, ToolMeta},
    types::{RequestContext, ToolAnnotations},
};
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
    /// The agent's id. Defaults to the one session live here.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[arg(long)]
    pub agent: Option<String>,
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

impl Tool<Control> for Title {
    type Output = String;

    async fn execute(self, control: Control, _context: RequestContext) -> Result<String> {
        match &control.executor {
            Executor::Client(state) => {
                let client = state.client(self.agent.as_deref()).await?;
                let identity = client.set_title(&self.title).await?;
                Ok(format!(
                    "{}: {}",
                    identity["id"].as_str().unwrap_or("session"),
                    identity["title"].as_str().unwrap_or_default()
                ))
            }
            Executor::Host(_) => Err(super::operator_verb("title")),
        }
    }
}
