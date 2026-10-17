use super::{Control, Executor, StateResult};
use anyhow::Result;
use mcplease::{
    traits::{Tool, ToolMeta},
    types::{RequestContext, ToolAnnotations},
};
use serde::{Deserialize, Serialize};

/// Print a session's whole `/state` body, without waiting: where the loop is
/// (`loop_phase`), what is in flight (`current_operations`), what each loop verb would
/// answer now (`verbs`), the pending gate and question, the inbox, the running
/// background processes, its usage, and its `status`.
#[derive(Debug, Serialize, Deserialize, schemars::JsonSchema, clap::Args)]
#[serde(rename = "state")]
pub struct State {
    /// The agent's id. Defaults to the session `--name` names, or the one live here.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[arg(long)]
    pub agent: Option<String>,
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

    async fn execute(self, control: Control, _context: RequestContext) -> Result<StateResult> {
        match &control.executor {
            Executor::Client(state) => {
                let client = state.client(self.agent.as_deref()).await?;
                StateResult::plain(&client.state().await?)
            }
            Executor::Host(_) => Err(super::operator_verb("state")),
        }
    }
}
