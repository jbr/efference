use super::{Control, Executor, pretty};
use anyhow::{Context, Result};
use mcplease::{
    traits::{Tool, ToolMeta, ToolOutput},
    types::{RequestContext, ToolAnnotations},
};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Create a session on the host and return its id. The session starts idle in
/// `cwd` with this process's environment as its base; send it a `turn` to begin.
/// Every knob is optional; an omitted model takes past picks, else creation
/// refuses naming the choice. The model is spelled `<connection>/<model>` as
/// `tui --new --model` takes it.
#[derive(Debug, Serialize, Deserialize, schemars::JsonSchema, clap::Args)]
#[serde(rename = "create")]
pub struct Create {
    /// The session's working directory. Relative to this process's; defaults to it.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[arg(long)]
    pub cwd: Option<PathBuf>,
    /// The session id, a single path segment. Minted (`adjective-noun`) when omitted.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[arg(long)]
    pub name: Option<String>,
    /// The main model, `<connection>/<model>`.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[arg(long)]
    pub model: Option<String>,
    /// The spotter model, `<connection>/<model>`.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[arg(long)]
    pub spotter_model: Option<String>,
    /// A config file to seed the session's `config.toml` from, instead of the project template.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[arg(long)]
    pub config: Option<PathBuf>,
    /// Reasoning effort for the main model, as the provider names its levels.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[arg(long)]
    pub effort: Option<String>,
    /// Sampling temperature, 0–100 (percent). 0 is (near-)deterministic; omitted
    /// uses the provider default (docs/transport.md).
    #[serde(skip_serializing_if = "Option::is_none")]
    #[arg(long)]
    #[schemars(range(min = 0, max = 100))]
    pub temperature: Option<u8>,
    /// Run without a human at the gate: calls the permission rungs do not clear are declined rather than held.
    #[serde(default)]
    #[arg(long)]
    pub autonomous: bool,
    /// Upstream session logs to seed from (`tui --new --upstream`). Repeatable.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[arg(long)]
    pub upstream: Vec<String>,
    /// Let the session outlive its creator. By default a session ends when the session that created it ends.
    #[serde(default)]
    #[arg(long)]
    pub detached: bool,
}

impl ToolMeta for Create {
    fn title() -> Option<&'static str> {
        Some("Create a session")
    }

    fn annotations() -> Option<ToolAnnotations> {
        Some(ToolAnnotations {
            read_only_hint: Some(false),
            destructive_hint: Some(false),
            // Two identical creates are two sessions.
            idempotent_hint: Some(false),
            open_world_hint: Some(false),
            ..Default::default()
        })
    }
}

/// A created session as a tool result: its id, the `session` every other tool
/// takes.
#[derive(Serialize, schemars::JsonSchema)]
pub struct CreatedResult {
    id: String,
}

impl ToolOutput for CreatedResult {
    fn output_schema() -> Option<serde_json::Value> {
        Some(mcplease::traits::schema_for::<Self>().into())
    }

    fn to_text(&self) -> String {
        pretty(self)
    }
}

impl Tool<Control> for Create {
    type Output = CreatedResult;

    async fn execute(self, control: Control, _context: RequestContext) -> Result<CreatedResult> {
        match &control.executor {
            Executor::Client(state) => {
                let here = std::env::current_dir().context("reading the working directory")?;
                let cwd = match self.cwd {
                    Some(cwd) => here.join(cwd),
                    None => here,
                };
                let env = crate::env_table::EnvSnapshot::capture()
                    .map_err(anyhow::Error::msg)?
                    .into_entries();
                let request = crate::daemon::CreateSession {
                    name: self.name,
                    cwd,
                    config: self.config,
                    // The agent-facing surface takes one opaque model slug;
                    // `connection` stays absent so it resolves as one
                    // (docs/control-api.md).
                    options: crate::inference::SessionOptions {
                        model: self.model,
                        spotter_model: self.spotter_model,
                        temperature: self.temperature,
                        effort: self.effort,
                        ..Default::default()
                    },
                    upstream: self.upstream,
                    autonomous: self.autonomous,
                    reauth: Vec::new(),
                    keep_scratch: false,
                    worktree: false,
                    detached: self.detached,
                    env: Some(env),
                };
                let host = state.host_client()?;
                let id = host.create_session(&request).await?;
                Ok(CreatedResult { id })
            }
            Executor::Host(state) => {
                // A session-driven creation runs in the caller's cwd and with the
                // caller's child environment as the new session's base — the same
                // environment its own bash children get (docs/tools.md).
                let caller = state.caller_session()?;
                let here = caller.cwd().to_path_buf();
                let cwd = crate::policy::lexical_normalize(&match self.cwd {
                    Some(cwd) => here.join(cwd),
                    None => here,
                });
                // Reach is the project (docs/tools.md): every later verb finds a
                // target only under the caller's `.efference/` base, so a child
                // minted anywhere else could never be supervised or ended by the
                // session that commissioned it. Refused here, before anything is
                // minted, rather than discovered at the first `state` call.
                let base = cwd.join(crate::daemon::SESSION_BASE);
                let project = caller
                    .storage_base()
                    .map(crate::policy::lexical_normalize)
                    .ok_or_else(|| anyhow::anyhow!("the calling session has no project base"))?;
                if base != project {
                    anyhow::bail!(
                        "refused: {} is outside this project — a session creates only under \
                         its own `.efference/` base ({}) so it can reach what it made \
                         (docs/tools.md)",
                        cwd.display(),
                        project.display()
                    );
                }
                let env = caller.child_env().resolved();
                let request = crate::daemon::CreateSession {
                    name: self.name,
                    cwd,
                    config: self.config,
                    options: crate::inference::SessionOptions {
                        model: self.model,
                        spotter_model: self.spotter_model,
                        temperature: self.temperature,
                        effort: self.effort,
                        ..Default::default()
                    },
                    upstream: self.upstream,
                    autonomous: self.autonomous,
                    reauth: Vec::new(),
                    keep_scratch: false,
                    worktree: false,
                    detached: self.detached,
                    env: Some(env),
                };
                // The child records the caller as its creator, which is what
                // makes it a descendant for later mutating calls — and
                // subscribes the caller to the child's status (docs/tools.md).
                let id = state.create(request).await?;
                Ok(CreatedResult { id })
            }
        }
    }
}
