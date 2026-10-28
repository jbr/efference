use super::args::turn_source;
use super::executor::{state_of, status_in};
use super::{Control, Executor};
use crate::daemon::status::answers;
use anyhow::Result;
use mcplease::{
    traits::{Tool, ToolMeta},
    types::{RequestContext, ToolAnnotations},
};
use serde::{Deserialize, Serialize};

/// Stop what an agent is doing now. A running tool is stopped (a command with its
/// process group) and the agent's turn ends; its next turn reads that you interrupted
/// it. A generation in flight ends and the agent goes idle.
#[derive(Debug, Serialize, Deserialize, schemars::JsonSchema, clap::Args)]
#[serde(rename = "interrupt")]
pub struct Interrupt {
    /// The agent's id.
    #[arg(long)]
    pub agent: String,
}

impl ToolMeta for Interrupt {
    fn title() -> Option<&'static str> {
        Some("Interrupt an agent")
    }

    fn annotations() -> Option<ToolAnnotations> {
        Some(ToolAnnotations {
            read_only_hint: Some(false),
            // A killed process group loses whatever it had not finished.
            destructive_hint: Some(true),
            idempotent_hint: Some(false),
            open_world_hint: Some(false),
            ..Default::default()
        })
    }
}

impl Tool<Control> for Interrupt {
    type Output = String;

    async fn execute(self, control: Control, context: RequestContext) -> Result<String> {
        let now = super::now();
        match &control.executor {
            Executor::Client(state) => {
                let client = state.client(Some(&self.agent)).await?;
                // What it stops, read just before signalling.
                let view = status_in(&state_of(&client).await?)?;
                let (answer, signal) = answers::interrupted(&view, now);
                if signal {
                    // A call over MCP names the mounting harness from its clientInfo;
                    // the command line's empty context is the operator's own
                    // `efference control interrupt`.
                    let source = turn_source(&context);
                    let control = source.is_none().then(|| "control-cli".to_string());
                    client
                        .interrupt(&crate::log::InterruptOrigin { source, control })
                        .await?;
                }
                Ok(answer)
            }
            Executor::Host(state) => {
                let (target, id) = state.resolve_driven(Some(&self.agent))?;
                let view = target.status_view(&id, &target.status());
                let (answer, signal) = answers::interrupted(&view, now);
                if signal {
                    // Attributed to the calling session, as its messages are (docs/tools.md).
                    target.interrupt(crate::log::InterruptOrigin {
                        source: Some(state.source_for(&target)),
                        control: None,
                    });
                }
                Ok(answer)
            }
        }
    }
}
