use super::Control;
use anyhow::Result;
use mcplease::{
    traits::{Tool, ToolMeta},
    types::{RequestContext, ToolAnnotations},
};
use serde::{Deserialize, Serialize};

/// Read what the session has said and done, one line per entry: the prose in
/// full, each tool call as its name, arguments and intent, each result as its
/// size, error flag and triage verdict — never a result's body. Every line starts
/// with the entry's `seq` (a call that shared its entry with others is `seq.n`, and
/// its result says `for seq.n`); `entries` fetches the whole entry behind any of
/// them by the seq before the dot, and `image` an image a result carried. Pass
/// `state`'s `last_seq` as `since` to read only what is new, or `tail` to read the
/// latest N entries; omit both for the whole session.
#[derive(Debug, Serialize, Deserialize, schemars::JsonSchema, clap::Args)]
#[serde(rename = "transcript")]
pub struct Transcript {
    /// Return only entries with `seq >= since`. Exclusive with `tail`.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[arg(long, conflicts_with = "tail")]
    pub since: Option<u64>,
    /// Return only the last `tail` entries. Exclusive with `since`.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[arg(long)]
    pub tail: Option<u64>,
    /// Read this session by id (see `sessions`). Required unless the server was started with `--name`/`--socket`; then it overrides that session for this call.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[arg(long)]
    pub session: Option<String>,
}

impl ToolMeta for Transcript {
    fn title() -> Option<&'static str> {
        Some("Read the session transcript")
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

impl Tool<Control> for Transcript {
    type Output = String;

    fn execute(self, state: &mut Control, _context: &RequestContext) -> Result<String> {
        let client = state.client(self.session.as_deref())?;
        let text = state.block_on(client.transcript(self.since, self.tail))?;
        Ok(if text.is_empty() {
            "(no entries)".to_string()
        } else {
            text
        })
    }
}
