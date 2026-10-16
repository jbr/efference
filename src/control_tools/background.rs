use super::{Control, Executor};
use anyhow::Result;
use mcplease::{
    traits::{Tool, ToolMeta},
    types::{RequestContext, ToolAnnotations},
};
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
    /// Act on this session by id (see `sessions`). Required unless the server was started with `--name`/`--socket`; then it overrides that session for this call.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[arg(long)]
    pub session: Option<String>,
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

impl Tool<Control> for Background {
    type Output = String;

    async fn execute(self, control: Control, _context: RequestContext) -> Result<String> {
        match &control.executor {
            Executor::Client(state) => {
                let client = state.client(self.session.as_deref()).await?;
                Ok(match client.background(self.note.as_deref()).await? {
                    Some(id) => format!(
                        "requested backgrounding p#{id} (lands only while it is still running)"
                    ),
                    None => "no foreground tool running; nothing to background".into(),
                })
            }
            Executor::Host(state) => {
                let session = state.resolve_mutating(self.session.as_deref())?;
                let foreground = session
                    .process_states()
                    .into_iter()
                    .find(|process| !process.background)
                    .map(|process| process.id);
                Ok(match foreground {
                    Some(id) => {
                        session.request_detach(self.note.clone());
                        format!(
                            "requested backgrounding p#{id} (lands only while it is still running)"
                        )
                    }
                    None => "no foreground tool running; nothing to background".into(),
                })
            }
        }
    }
}
