use super::{Control, Executor};
use anyhow::{Result, anyhow};
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
    /// `upstream` — the spotter judges first and escalates to the session that
    /// created this one, which answers the gate with `approve`; with no live
    /// creator this reviews as `spotter_human`. The default for a session another
    /// session created, and `spotter_human` is how you hand one back to the operator.
    /// `spotter_human` — the spotter judges first and escalates to a human gate.
    /// `human_only` — skip the spotter; every uncleared call gates.
    pub level: String,
    /// Act on this session by id (see `sessions`). Required unless the server was started with `--name`/`--socket`; then it overrides that session for this call.
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

    async fn execute(self, control: Control, _context: RequestContext) -> Result<String> {
        match &control.executor {
            Executor::Client(state) => {
                let client = state.client(self.session.as_deref()).await?;
                let set = client.review_level(&self.level).await?;
                Ok(format!("review level: {set}"))
            }
            Executor::Host(state) => {
                let session = state.resolve_mutating(self.session.as_deref())?;
                let level = crate::policy::ReviewLevel::parse(&self.level).ok_or_else(|| {
                    anyhow!(
                        "review_level must be autonomous | upstream | spotter_human | human_only"
                    )
                })?;
                session.set_review_level(level);
                Ok(format!("review level: {}", level.as_str()))
            }
        }
    }
}
