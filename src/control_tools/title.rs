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
    /// Act on this session by id (see `sessions`). Required unless the server was started with `--name`/`--socket`; then it overrides that session for this call.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[arg(long)]
    pub session: Option<String>,
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

    fn execute(self, control: &mut Control, _context: &RequestContext) -> Result<String> {
        let Executor::Client(state) = &mut control.executor;
        let client = state.client(self.session.as_deref())?;
        let identity = state.block_on(client.set_title(&self.title))?;
        Ok(format!(
            "{}: {}",
            identity["id"].as_str().unwrap_or("session"),
            identity["title"].as_str().unwrap_or_default()
        ))
    }
}
