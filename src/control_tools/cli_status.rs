//! `status` as `efference control` serves it: the same read, plus `--follow` and
//! `--output json` (docs/operating.md). The in-session and MCP `status` take neither, so
//! the command line has its own struct, as the MCP set does for `follow`.

use super::args::reader_name;
use super::executor::{state_of, status_in};
use super::follow::follow_status;
use super::verb::{ControlVerb, VerbCall};
use super::{ClientExecutor, HostExecutor, Status};
use anyhow::{Result, anyhow};
use mcplease::{traits::ToolMeta, types::ToolAnnotations};
use serde::{Deserialize, Serialize};

/// How `status` prints.
#[derive(
    Debug,
    Default,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Serialize,
    Deserialize,
    schemars::JsonSchema,
    clap::ValueEnum,
)]
#[serde(rename_all = "snake_case")]
pub enum Output {
    /// The rendered status.
    #[default]
    Text,
    /// The status view as JSON: one object for an agent, an array for the overview.
    Json,
}

/// Where an agent stands and the call that answers it. With no agent, one line for
/// each agent in view.
#[derive(Debug, Serialize, Deserialize, schemars::JsonSchema, clap::Args)]
#[serde(rename = "status")]
pub struct CliStatus {
    /// The agent's id; omitted, every agent in view. With `--follow` it repeats, one
    /// per followed agent.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[arg(long)]
    pub agent: Vec<String>,
    /// Print each followed agent's status, then each new decision point, until every
    /// one has ended; exits non-zero when the host stops answering.
    #[serde(skip)]
    #[schemars(skip)]
    #[arg(long)]
    pub follow: bool,
    /// `text` (default) or `json`.
    #[serde(skip)]
    #[schemars(skip)]
    #[arg(long, value_enum, default_value_t)]
    pub output: Output,
}

impl ToolMeta for CliStatus {
    fn title() -> Option<&'static str> {
        Status::title()
    }

    fn annotations() -> Option<ToolAnnotations> {
        Status::annotations()
    }
}

impl CliStatus {
    /// The agent a single read names, refusing `--follow` beside `--output json` and a
    /// repeated `--agent` without `--follow`.
    fn read_agent(&self) -> Result<Option<String>> {
        if self.follow && self.output == Output::Json {
            return Err(anyhow!(
                "`--follow` prints rendered blocks; `--output json` is for one read"
            ));
        }
        if self.agent.len() > 1 && !self.follow {
            return Err(anyhow!("`--agent` repeats only with `--follow`"));
        }
        Ok(self.agent.first().cloned())
    }

    /// Print the followed agents' blocks on stdout as they arrive; nothing is left to
    /// print when this returns.
    async fn follow(self, state: &ClientExecutor, call: &VerbCall<'_>) -> Result<String> {
        let agents = match self.agent {
            agents if !agents.is_empty() => agents,
            _ if state.host_wide() => {
                return Err(anyhow!("`--follow` streams named agents: pass `--agent`"));
            }
            _ => vec![
                state
                    .client(None)
                    .await?
                    .session_id()
                    .map(str::to_string)
                    .ok_or_else(|| {
                        anyhow!("`--follow` streams an agent on the host: pass `--agent`")
                    })?,
            ],
        };
        let me = reader_name(call.context);
        follow_status(
            &state.host_client()?,
            &agents,
            me.as_deref(),
            &mut std::io::stdout(),
        )
        .await?;
        Ok(String::new())
    }
}

impl ControlVerb for CliStatus {
    type Output = String;

    async fn client(self, state: &ClientExecutor, call: &VerbCall<'_>) -> Result<String> {
        let agent = self.read_agent()?;
        if self.follow {
            return self.follow(state, call).await;
        }
        match self.output {
            Output::Text => {
                Status {
                    agent: agent.into(),
                }
                .client(state, call)
                .await
            }
            Output::Json => json(state, agent.as_deref()).await,
        }
    }

    async fn host(self, state: &HostExecutor, call: &VerbCall<'_>) -> Result<String> {
        let agent = self.read_agent()?;
        if self.follow {
            return Err(anyhow!("`--follow` is served by `efference control`"));
        }
        match self.output {
            Output::Text => {
                Status {
                    agent: agent.into(),
                }
                .host(state, call)
                .await
            }
            Output::Json => Err(anyhow!("`--output json` is served by `efference control`")),
        }
    }
}

/// The status view of `agent` as JSON, or an array of every session the host holds.
async fn json(state: &ClientExecutor, agent: Option<&str>) -> Result<String> {
    if agent.is_none() && state.host_wide() {
        let host = state.host_client()?;
        let mut views = Vec::new();
        for row in host.sessions().await? {
            let client = host.clone().with_prefix(&format!("/sessions/{}", row.id));
            if let Ok(view) = status_in(&state_of(&client).await?) {
                views.push(view);
            }
        }
        return Ok(serde_json::to_string_pretty(&views)?);
    }
    let client = state.client(agent).await?;
    let view = status_in(&state_of(&client).await?)?;
    Ok(serde_json::to_string_pretty(&view)?)
}
