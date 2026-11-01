use super::args::{NamedAgent, turn_source};
use super::executor::{state_of, status_in};
use super::verb::{ControlVerb, VerbCall};
use super::{ClientExecutor, HostExecutor};
use crate::daemon::status::answers;
use anyhow::Result;
use mcplease::{traits::ToolMeta, types::ToolAnnotations};
use serde::{Deserialize, Serialize};

/// Stop what an agent is doing now. A running tool is stopped (a command with its
/// process group) and the agent's turn ends; its next turn reads that you interrupted
/// it. A generation in flight ends and the agent goes idle.
#[derive(Debug, Serialize, Deserialize, schemars::JsonSchema, clap::Args)]
#[serde(rename = "interrupt")]
pub struct Interrupt {
    #[command(flatten)]
    pub agent: NamedAgent,
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

impl ControlVerb for Interrupt {
    type Output = String;

    async fn client(self, state: &ClientExecutor, call: &VerbCall<'_>) -> Result<String> {
        let now = super::now();
        let client = state.client(Some(self.agent.id())).await?;
        // What it stops, read just before signalling.
        let view = status_in(&state_of(&client).await?)?;
        let (answer, signal) = answers::interrupted(&view, now);
        if signal {
            // A call over MCP names the mounting harness from its clientInfo; the command
            // line's empty context is the operator's own `efference control interrupt`.
            let source = turn_source(call.context);
            let control = source.is_none().then(|| "control-cli".to_string());
            client
                .interrupt(&crate::record::log::InterruptOrigin { source, control })
                .await?;
        }
        Ok(answer)
    }

    async fn host(self, state: &HostExecutor, _call: &VerbCall<'_>) -> Result<String> {
        let now = super::now();
        let (target, id) = state.resolve_driven(Some(self.agent.id()))?;
        let view = target.status_view(&id, &target.status());
        let (answer, signal) = answers::interrupted(&view, now);
        if signal {
            // Attributed to the calling session, as its messages are (docs/tools.md).
            target.interrupt(crate::record::log::InterruptOrigin {
                source: Some(state.source_for(&target)),
                control: None,
            });
        }
        Ok(answer)
    }
}
