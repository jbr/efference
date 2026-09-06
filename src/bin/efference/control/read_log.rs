use super::{Control, LogResult};
use anyhow::Result;
use mcplease::{
    traits::{Tool, ToolMeta},
    types::{RequestContext, ToolAnnotations},
};
use serde::{Deserialize, Serialize};

/// Read the session's durable append-only log — every turn, tool call, result,
/// permission verdict and config edit, in order, each with a `seq`. Pass the last
/// `seq` you saw as `since` to read only what is new; a whole log can be large.
#[derive(Debug, Serialize, Deserialize, schemars::JsonSchema, clap::Args)]
#[serde(rename = "log")]
pub struct Log {
    /// Return only entries with `seq >= since`.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[arg(long)]
    pub since: Option<u64>,
    /// Read this session instead of the resolved one — the per-call override of the session resolved from `--name`/`--socket`/discovery.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[arg(long)]
    pub session: Option<String>,
}

impl ToolMeta for Log {
    fn title() -> Option<&'static str> {
        Some("Read the session log")
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

impl Tool<Control> for Log {
    type Output = LogResult;

    fn execute(self, state: &mut Control, _context: &RequestContext) -> Result<LogResult> {
        let client = state.client(self.session.as_deref())?;
        LogResult::new(&state.block_on(client.log(self.since))?)
    }
}
