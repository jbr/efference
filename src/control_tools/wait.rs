use super::args::reader_name;
use super::executor::{state_of, status_in};
use super::status::operator_reader;
use super::watch::Next;
use super::{Control, Executor};
use crate::daemon::status::{answers, render};
use anyhow::{Result, anyhow};
use mcplease::{
    traits::{Tool, ToolMeta},
    types::{RequestContext, ToolAnnotations},
};
use serde::{Deserialize, Serialize};
use std::time::Duration;

/// How long a `wait` that names no `timeout_s` waits, in seconds (docs/operating.md).
const DEFAULT_TIMEOUT_S: u64 = 100;

/// Wait for a status you have not seen from the agents you created or named with
/// `agent`, and answer with every new one: a gate, a question, its turn ending, a stop,
/// or its end. Returns at once when one is unread; an agent working is never new.
#[derive(Debug, Serialize, Deserialize, schemars::JsonSchema, clap::Args)]
#[serde(rename = "wait")]
pub struct Wait {
    /// An agent to watch from now on, beside the ones you created.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[arg(long)]
    pub agent: Option<String>,
    /// Seconds to wait for a new status before answering with where each agent is
    /// now; 100 when omitted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[arg(long)]
    pub timeout_s: Option<u64>,
}

impl ToolMeta for Wait {
    fn title() -> Option<&'static str> {
        Some("Wait for a new status")
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

impl Tool<Control> for Wait {
    type Output = String;

    async fn execute(self, control: Control, context: RequestContext) -> Result<String> {
        // Only the MCP set lists `wait`: a session hears its agents through its inbox,
        // and the command line holds no mount to watch from (docs/tools.md).
        let Executor::Client(state) = &control.executor else {
            return Err(anyhow!(
                "`wait` is served by `efference mcp`; a session hears its agents through \
                 its inbox"
            ));
        };
        if let Some(agent) = self.agent.as_deref() {
            state.watch_named(agent).await?;
        }
        let seconds = self.timeout_s.unwrap_or(DEFAULT_TIMEOUT_S);
        let me = reader_name(&context);
        let me = me.as_deref();
        match state.watched().next(Duration::from_secs(seconds)).await {
            Next::Nothing => Err(anyhow!(
                "you watch no agents: create one, or name one with `agent`"
            )),
            Next::New(views) => {
                let now = super::now();
                Ok(views
                    .iter()
                    .map(|view| render(view, &operator_reader(view, me), now))
                    .collect::<Vec<_>>()
                    .join("\n\n"))
            }
            Next::Unchanged(agents) => {
                let now = super::now();
                let mut lines = Vec::new();
                for (id, latest) in agents {
                    // A follower that has read nothing yet: the status now, read once.
                    let view = match latest {
                        Some(view) => Ok(view),
                        None => async { status_in(&state_of(&state.hosted(&id)?).await?) }.await,
                    };
                    lines.push(match view {
                        Ok(view) => answers::first_line(&view, &operator_reader(&view, me), now),
                        Err(e) => format!("{id}: its status could not be read: {e:#}"),
                    });
                }
                Ok(answers::unchanged(seconds, lines))
            }
        }
    }
}
