use super::Control;
use anyhow::Result;
use mcplease::{
    traits::{Tool, ToolMeta},
    types::{RequestContext, ToolAnnotations},
};
use serde::{Deserialize, Serialize};

/// Withdraw a pending notification before the session triages it. Ids come from
/// `notify`'s answer or from the inbox in `state`.
#[derive(Debug, Serialize, Deserialize, schemars::JsonSchema, clap::Args)]
#[serde(rename = "dismiss")]
pub struct Dismiss {
    /// The notification id.
    pub id: u64,
    /// Act on this session instead of the resolved one — the per-call override of the session resolved from `--name`/`--socket`/discovery.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[arg(long)]
    pub session: Option<String>,
}

impl ToolMeta for Dismiss {
    fn title() -> Option<&'static str> {
        Some("Dismiss a notification")
    }

    fn annotations() -> Option<ToolAnnotations> {
        Some(ToolAnnotations {
            read_only_hint: Some(false),
            destructive_hint: Some(false),
            // Dismissing an already-dismissed id is not a second effect.
            idempotent_hint: Some(true),
            open_world_hint: Some(false),
            ..Default::default()
        })
    }
}

impl Tool<Control> for Dismiss {
    type Output = String;

    fn execute(self, state: &mut Control, _context: &RequestContext) -> Result<String> {
        let client = state.client(self.session.as_deref())?;
        state.block_on(client.dismiss_notification(self.id))?;
        Ok(format!("dismissed {}", self.id))
    }
}
