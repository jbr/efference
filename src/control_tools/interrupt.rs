use super::{Control, Executor};
use anyhow::Result;
use mcplease::{
    traits::{Tool, ToolMeta},
    types::{RequestContext, ToolAnnotations},
};
use serde::{Deserialize, Serialize};

/// Stop what the session is doing now. A running tool is killed with its process
/// group and the turn continues from the killed call's result; a generation in
/// flight is ended and the loop returns to idle, where a queued turn lands at once.
/// No-op only when the loop is idle. To keep a long-running command instead of
/// killing it, use `background`.
#[derive(Debug, Serialize, Deserialize, schemars::JsonSchema, clap::Args)]
#[serde(rename = "interrupt")]
pub struct Interrupt {
    /// Interrupt this session by id (see `sessions`). Required unless the server was started with `--name`/`--socket`; then it overrides that session for this call.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[arg(long)]
    pub session: Option<String>,
}

impl ToolMeta for Interrupt {
    fn title() -> Option<&'static str> {
        Some("Interrupt the running tool")
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
        match &control.executor {
            Executor::Client(state) => {
                let client = state.client(self.session.as_deref()).await?;
                // As a turn does: a call over MCP names the mounting harness from its
                // clientInfo; the command line's empty context is the operator's own
                // `efference control interrupt`.
                let source = super::turn::turn_source(None, &context);
                let control = source.is_none().then(|| "control-cli".to_string());
                client
                    .interrupt(&crate::log::InterruptOrigin { source, control })
                    .await?;
                Ok("interrupted".into())
            }
            Executor::Host(state) => {
                // Attributed to the calling session, as its turns are (docs/tools.md).
                state
                    .resolve_mutating(self.session.as_deref())?
                    .interrupt(crate::log::InterruptOrigin {
                        source: Some(format!("session[{}]", state.caller)),
                        control: None,
                    });
                Ok("interrupted".into())
            }
        }
    }
}
