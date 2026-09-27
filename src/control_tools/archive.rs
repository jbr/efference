use super::{Control, Executor};
use anyhow::Result;
use mcplease::{
    traits::{Tool, ToolMeta},
    types::{RequestContext, ToolAnnotations},
};
use serde::{Deserialize, Serialize};

/// Archive a hosted session: its loop stops, its background processes are reaped,
/// and its scratch is removed. Returns once that teardown has run. The session's
/// log stays on disk, which is what a later resume reads; until resumption exists
/// an archived session leaves the host's list and does not run again.
#[derive(Debug, Serialize, Deserialize, schemars::JsonSchema, clap::Args)]
#[serde(rename = "archive")]
pub struct Archive {
    /// The session to archive, by id (see `sessions`).
    pub session: String,
}

impl ToolMeta for Archive {
    fn title() -> Option<&'static str> {
        Some("Archive a session")
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
                host.archive_session(&self.session).await?;
                Ok(format!("archived {}", self.session))
            }
            Executor::Host(state) => {
                // Archiving is mutating: the target must be a descendant, or
                // the caller's own predecessor.
                state.resolve_archive(Some(&self.session))?;
                state.archive(&self.session).await?;
                Ok(format!("archived {}", self.session))
            }
        }
    }
}
