use super::{Control, EntriesResult, Executor};
use anyhow::{Context as _, Result, anyhow};
use mcplease::{
    traits::{Tool, ToolMeta},
    types::{RequestContext, ToolAnnotations},
};
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
    /// Read this session by id (see `sessions`). Required unless the server was started with `--name`/`--socket`; then it overrides that session for this call.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[arg(long)]
    pub session: Option<String>,
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

impl Tool<Control> for Entries {
    type Output = EntriesResult;

    fn execute(self, control: &mut Control, _context: &RequestContext) -> Result<EntriesResult> {
        match &mut control.executor {
            Executor::Client(state) => {
                let client = state.client(self.session.as_deref())?;
                let mut entries = Vec::with_capacity(self.seqs.len());
                for seq in self.seqs {
                    let body = state
                        .block_on(client.log_entry(seq))
                        .with_context(|| format!("entry {seq}"))?;
                    entries.push(serde_json::from_str(&body)?);
                }
                Ok(EntriesResult { entries })
            }
            Executor::Host(state) => {
                let session = state.resolve(self.session.as_deref())?;
                let mut entries = Vec::with_capacity(self.seqs.len());
                for seq in &self.seqs {
                    let body = session
                        .entry_json(*seq)
                        .ok_or_else(|| anyhow!("entry {seq}"))?;
                    entries.push(serde_json::from_str(&body)?);
                }
                Ok(EntriesResult { entries })
            }
        }
    }
}
