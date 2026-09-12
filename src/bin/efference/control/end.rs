use super::Control;
use anyhow::Result;
use mcplease::{
    traits::{Tool, ToolMeta},
    types::{RequestContext, ToolAnnotations},
};
use serde::{Deserialize, Serialize};

/// End a hosted session: its loop stops, its background processes are reaped, and
/// its scratch is removed. Returns once that teardown has run. The session's log
/// stays on disk; the session itself is gone from the host and cannot be resumed.
#[derive(Debug, Serialize, Deserialize, schemars::JsonSchema, clap::Args)]
#[serde(rename = "end")]
pub struct End {
    /// The session to end, by id (see `sessions`).
    pub session: String,
}

impl ToolMeta for End {
    fn title() -> Option<&'static str> {
        Some("End a session")
    }

    fn annotations() -> Option<ToolAnnotations> {
        Some(ToolAnnotations {
            read_only_hint: Some(false),
            destructive_hint: Some(true),
            // Ending an ended session is a `404`, not a no-op.
            idempotent_hint: Some(false),
            open_world_hint: Some(false),
            ..Default::default()
        })
    }
}

impl Tool<Control> for End {
    type Output = String;

    fn execute(self, state: &mut Control, _context: &RequestContext) -> Result<String> {
        let host = state.host_client()?;
        state.block_on(host.end_session(&self.session))?;
        Ok(format!("ended {}", self.session))
    }
}
