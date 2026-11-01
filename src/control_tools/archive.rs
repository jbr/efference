use super::verb::{ControlVerb, VerbCall};
use super::{ClientExecutor, HostExecutor};
use crate::session::status::answers;
use anyhow::{Result, anyhow};
use mcplease::{traits::ToolMeta, types::ToolAnnotations};
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

impl ControlVerb for Archive {
    type Output = String;

    async fn client(self, state: &ClientExecutor, call: &VerbCall<'_>) -> Result<String> {
        let host = state.host_client()?;
        // A call over MCP names the mounting harness from its clientInfo; the command
        // line's empty context is the operator's own.
        let source = super::args::turn_source(call.context);
        let ended_with = host.archive_session(&self.agent, source.as_deref()).await?;
        Ok(answers::archived(&self.agent, &ended_with))
    }

    async fn host(self, state: &HostExecutor, _call: &VerbCall<'_>) -> Result<String> {
        // Archiving drives the target: a descendant, a supervised session, or the
        // caller's own predecessor.
        let (target, id) = state.resolve_driven(Some(&self.agent))?;
        let host = state.host()?;
        // Read before the end starts, while the children it cascades to are still
        // listed.
        let ended_with = host.ending_with(&id);
        // Attributed to the calling session, as its messages are (docs/tools.md).
        let source = state.source_for(&target);
        let done = host
            .end_by(&id, Some(source))
            .ok_or_else(|| anyhow!("no agent {id:?} to archive"))?;
        done.await;
        Ok(answers::archived(&id, &ended_with))
    }
}
