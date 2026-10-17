use super::{Control, Executor};
use crate::daemon::status::answers;
use anyhow::{Result, anyhow};
use mcplease::{
    traits::{Tool, ToolMeta},
    types::{RequestContext, ToolAnnotations},
};
use serde::{Deserialize, Serialize};

/// End an agent, and the agents it created that end with it. Their logs stay on
/// disk.
#[derive(Debug, Serialize, Deserialize, schemars::JsonSchema, clap::Args)]
#[serde(rename = "archive")]
pub struct Archive {
    /// The agent's id.
    pub agent: String,
}

impl ToolMeta for Archive {
    fn title() -> Option<&'static str> {
        Some("Archive an agent")
    }

    fn annotations() -> Option<ToolAnnotations> {
        Some(ToolAnnotations {
            read_only_hint: Some(false),
            destructive_hint: Some(true),
            // Archiving an archived session is a `404`, not a no-op.
            idempotent_hint: Some(false),
            open_world_hint: Some(false),
            ..Default::default()
        })
    }
}

impl Tool<Control> for Archive {
    type Output = String;

    async fn execute(self, control: Control, _context: RequestContext) -> Result<String> {
        match &control.executor {
            Executor::Client(state) => {
                let host = state.host_client()?;
                let ended_with = host.archive_session(&self.agent).await?;
                Ok(answers::archived(&self.agent, &ended_with))
            }
            Executor::Host(state) => {
                // Archiving drives the target: a descendant, a supervised session, or
                // the caller's own predecessor.
                let (_, id) = state.resolve_driven(Some(&self.agent))?;
                let host = state.host()?;
                // Read before the end starts, while the children it cascades to are
                // still listed.
                let ended_with = host.ending_with(&id);
                let done = host
                    .end(&id)
                    .ok_or_else(|| anyhow!("no agent {id:?} to archive"))?;
                done.await;
                Ok(answers::archived(&id, &ended_with))
            }
        }
    }
}
