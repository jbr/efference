use super::{Control, Executor};
use anyhow::Result;
use mcplease::{
    traits::{Tool, ToolMeta},
    types::{RequestContext, ToolAnnotations},
};
use serde::{Deserialize, Serialize};

/// Set who reviews a tool call the permission fast path did not clear.
#[derive(Debug, Serialize, Deserialize, schemars::JsonSchema, clap::Args)]
#[serde(rename = "review_level")]
pub struct ReviewLevel {
    /// `autonomous` — the spotter hands an unclear call back to the model with a
    /// pointer at its config file, so nothing blocks on an absent human.
    /// `supervisor` — the spotter judges first and escalates to this session's
    /// supervisor (by default the session that created it), which answers the gate
    /// with `decide`; with no live supervisor this reviews as `spotter_human`. The
    /// default for a session another session created, and `spotter_human` is how you
    /// hand one back to the operator.
    /// `spotter_human` — the spotter judges first and escalates to a human gate.
    /// `human_only` — skip the spotter; every uncleared call gates.
    pub level: String,
    /// The agent's id. Defaults to the session `--name` names, or the one live here.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[arg(long)]
    pub agent: Option<String>,
}

impl ToolMeta for ReviewLevel {
    fn title() -> Option<&'static str> {
        Some("Set the review level")
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

impl Tool<Control> for ReviewLevel {
    type Output = String;

    async fn execute(self, control: Control, _context: RequestContext) -> Result<String> {
        match &control.executor {
            Executor::Client(state) => {
                let client = state.client(self.agent.as_deref()).await?;
                let set = client.review_level(&self.level).await?;
                Ok(format!("review level: {set}"))
            }
            Executor::Host(_) => Err(super::operator_verb("review_level")),
        }
    }
}
