use super::args::reader_name;
use super::executor::status_in;
use super::results::{StateResult, pretty};
use super::status::operator_relation;
use super::{Control, Executor};
use crate::daemon::status::{Moves, Reader, render};
use anyhow::Result;
use mcplease::{
    traits::{Tool, ToolMeta},
    types::{RequestContext, ToolAnnotations},
};
use serde::{Deserialize, Serialize};

/// Wait until an agent reaches its next decision point — a held call, a question,
/// its turn ending, or a stop that waits on someone — and answer with its status.
/// Returns at once if it is already there.
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

impl Tool<Control> for Settle {
    type Output = String;

    async fn execute(self, control: Control, context: RequestContext) -> Result<String> {
        match &control.executor {
            Executor::Client(state) => {
                let client = state.client(self.agent.as_deref()).await?;
                let waited = state
                    .settle_within(&client, self.timeout_s, client.settle())
                    .await?;
                // The command line prints the `/state` body; an agent reads the status.
                if control.is_cli() {
                    return Ok(pretty(&StateResult::waited(&waited.body, waited.settled)?));
                }
                let view = status_in(&serde_json::from_str(&waited.body)?)?;
                let me = reader_name(&context);
                let reader = Reader::read(operator_relation(&view), &Moves::MCP, me.as_deref());
                let status = render(&view, &reader, super::now());
                Ok(match (waited.settled, self.timeout_s) {
                    (true, _) => status,
                    (false, Some(seconds)) => {
                        format!("still going after {seconds}s; where it is now:\n{status}")
                    }
                    (false, None) => format!("still going; where it is now:\n{status}"),
                })
            }
            // Not mounted in a session: a session has an inbox to be woken through,
            // not a blocking wait (docs/tools.md).
            Executor::Host(_) => Err(super::operator_verb("settle")),
        }
    }
}
