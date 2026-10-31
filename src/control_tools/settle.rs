use super::ClientExecutor;
use super::results::{StateResult, pretty};
use super::verb::{ControlVerb, VerbCall};
use anyhow::Result;
use mcplease::{traits::ToolMeta, types::ToolAnnotations};
use serde::{Deserialize, Serialize};

/// Wait until an agent reaches its next decision point — a held call, a question,
/// its turn ending, or a stop that waits on someone — and print its `/state`.
/// Returns at once if it is already there.
// A command-line verb; the MCP mount waits with `wait` (docs/operating.md).
#[derive(Debug, Serialize, Deserialize, schemars::JsonSchema, clap::Args)]
#[serde(rename = "settle")]
pub struct Settle {
    /// Seconds to wait before answering with where it is then. Omitted, the wait
    /// has no bound.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[arg(long)]
    pub timeout_s: Option<u64>,
    /// The agent's id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[arg(long)]
    pub agent: Option<String>,
}

impl ToolMeta for Settle {
    fn title() -> Option<&'static str> {
        Some("Wait for an agent to settle")
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

// Not mounted in a session: a session has an inbox to be woken through, not a blocking
// wait (docs/tools.md).
impl ControlVerb for Settle {
    type Output = String;

    async fn client(self, state: &ClientExecutor, _call: &VerbCall<'_>) -> Result<String> {
        let client = state.client(self.agent.as_deref()).await?;
        let waited = state
            .settle_within(&client, self.timeout_s, client.settle())
            .await?;
        Ok(pretty(&StateResult::waited(&waited.body, waited.settled)?))
    }
}
