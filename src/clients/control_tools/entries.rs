//! The `entries` session-control verb (docs/tools.md).

use super::args::AgentOrHere;
use super::verb::{ControlVerb, VerbCall};
use super::{ClientExecutor, EntriesResult};
use anyhow::{Context as _, Result};
use mcplease::{traits::ToolMeta, types::ToolAnnotations};
use serde::{Deserialize, Serialize};

/// Fetch whole log entries by `seq` — the full record behind a `transcript` line:
/// a result's complete body, a call's exact arguments, a verdict's every field.
/// Each entry can be large; ask for the ones you need, not a range. An image on a
/// result is referenced, not inlined; `image` fetches its bytes.
#[derive(Debug, Serialize, Deserialize, schemars::JsonSchema, clap::Args)]
#[serde(rename = "entries")]
pub struct Entries {
    /// The `seq`s to fetch, as `transcript` lines show them.
    #[arg(required = true, num_args = 1..)]
    pub seqs: Vec<u64>,
    #[serde(default, skip_serializing_if = "AgentOrHere::is_none")]
    #[command(flatten)]
    pub agent: AgentOrHere,
}

impl ToolMeta for Entries {
    fn title() -> Option<&'static str> {
        Some("Fetch log entries by seq")
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

impl ControlVerb for Entries {
    type Output = EntriesResult;

    async fn client(self, state: &ClientExecutor, _call: &VerbCall<'_>) -> Result<EntriesResult> {
        let client = state.client(self.agent.id()).await?;
        let mut entries = Vec::with_capacity(self.seqs.len());
        for seq in self.seqs {
            let body = client
                .log_entry(seq)
                .await
                .with_context(|| format!("entry {seq}"))?;
            entries.push(serde_json::from_str(&body)?);
        }
        Ok(EntriesResult { entries })
    }
}
