use super::Control;
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
    /// `spotter_human` — the spotter judges first and escalates to a human gate.
    /// `human_only` — skip the spotter; every uncleared call gates.
    pub level: String,
    /// Act on this session instead of the resolved one — the per-call override of the session resolved from `--name`/`--socket`/discovery.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[arg(long)]
    pub session: Option<String>,
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

    fn execute(self, state: &mut Control, _context: &RequestContext) -> Result<String> {
        let client = state.client(self.session.as_deref())?;
        let set = state.block_on(client.review_level(&self.level))?;
        Ok(format!("review level: {set}"))
    }
}
