use super::{Control, StateResult};
use anyhow::Result;
use mcplease::{
    traits::{Tool, ToolMeta},
    types::{RequestContext, ToolAnnotations},
};
use serde::{Deserialize, Serialize};

/// Read where the session is right now, without waiting: the pending gate if there
/// is one (with the held call's id), what the loop is doing, the inbox, the running
/// background processes, and the session's usage. This is the call to make first
/// when you do not know what a session is up to.
#[derive(Debug, Serialize, Deserialize, schemars::JsonSchema, clap::Args)]
#[serde(rename = "state")]
pub struct State {
    /// Read this session instead of the resolved one — the per-call override of the session resolved from `--name`/`--socket`/discovery.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[arg(long)]
    pub session: Option<String>,
}

impl ToolMeta for State {
    fn title() -> Option<&'static str> {
        Some("Read session state")
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

impl Tool<Control> for State {
    type Output = StateResult;

    fn execute(self, state: &mut Control, _context: &RequestContext) -> Result<StateResult> {
        let client = state.client(self.session.as_deref())?;
        StateResult::plain(&state.block_on(client.state())?)
    }
}
