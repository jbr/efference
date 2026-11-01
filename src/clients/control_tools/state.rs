//! The `state` session-control verb (docs/tools.md).

use super::args::AgentOrHere;
use super::verb::{ControlVerb, VerbCall};
use super::{ClientExecutor, StateResult};
use anyhow::Result;
use mcplease::{traits::ToolMeta, types::ToolAnnotations};
use serde::{Deserialize, Serialize};

/// Print a session's whole `/state` body, without waiting: where the loop is
/// (`loop_phase`), what is in flight (`current_operations`), what each loop verb would
/// answer now (`verbs`), the pending gate and question, the inbox, the running
/// background processes, its usage, and its `status`.
#[derive(Debug, Serialize, Deserialize, schemars::JsonSchema, clap::Args)]
#[serde(rename = "state")]
pub struct State {
    #[serde(default, skip_serializing_if = "AgentOrHere::is_none")]
    #[command(flatten)]
    pub agent: AgentOrHere,
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

impl ControlVerb for State {
    type Output = StateResult;

    async fn client(self, state: &ClientExecutor, _call: &VerbCall<'_>) -> Result<StateResult> {
        let client = state.client(self.agent.id()).await?;
        StateResult::plain(&client.state().await?)
    }
}
