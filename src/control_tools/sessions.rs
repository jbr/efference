use super::{Control, Executor, pretty};
use anyhow::Result;
use crate::client::SessionListing;
use mcplease::{
    traits::{Tool, ToolMeta, ToolOutput},
    types::{RequestContext, ToolAnnotations},
};
use serde::{Deserialize, Serialize};

/// List the sessions the host holds: each id, and its one-line title once one has
/// been written. The id is what every other tool's `session` takes. This is the
/// call to make first when you do not know what is running.
#[derive(Debug, Serialize, Deserialize, schemars::JsonSchema, clap::Args)]
#[serde(rename = "sessions")]
pub struct Sessions {}

impl ToolMeta for Sessions {
    fn title() -> Option<&'static str> {
        Some("List the host's sessions")
    }

    fn annotations() -> Option<ToolAnnotations> {
        Some(ToolAnnotations {
            read_only_hint: Some(true),
            destructive_hint: Some(false),
            idempotent_hint: Some(true),
            open_world_hint: Some(false),
            ..Default::default()
        })
    }
}

/// The host's session list as a tool result (docs/control-api.md).
#[derive(Serialize, schemars::JsonSchema)]
pub struct SessionsResult {
    /// The hosted sessions, sorted by id.
    sessions: Vec<SessionListing>,
}

impl ToolOutput for SessionsResult {
    fn output_schema() -> Option<serde_json::Value> {
        Some(mcplease::traits::schema_for::<Self>().into())
    }

    fn to_text(&self) -> String {
        pretty(self)
    }
}

impl Tool<Control> for Sessions {
    type Output = SessionsResult;

    fn execute(self, control: &mut Control, _context: &RequestContext) -> Result<SessionsResult> {
        let Executor::Client(state) = &mut control.executor;
        let host = state.host_client()?;
        let sessions = state.block_on(host.sessions())?;
        Ok(SessionsResult { sessions })
    }
}
