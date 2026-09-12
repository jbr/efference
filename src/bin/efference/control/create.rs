use super::{Control, pretty};
use anyhow::{Context, Result};
use mcplease::{
    traits::{Tool, ToolMeta, ToolOutput},
    types::{RequestContext, ToolAnnotations},
};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Create a session on the host and return its id. The session starts idle in
/// `cwd` with this process's environment as its base; send it a `turn` to begin.
/// Every knob is optional and the host's defaults fill omissions; the model is
/// spelled `<connection>/<native model>` as `tui --new --model` takes it.
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
    /// Sampling temperature, 0–100 (percent).
    #[serde(skip_serializing_if = "Option::is_none")]
    #[arg(long)]
    pub temperature: Option<u8>,
    /// Run without a human at the gate: calls the permission rungs do not clear are declined rather than held.
    #[serde(default)]
    #[arg(long)]
    pub autonomous: bool,
    /// Upstream session logs to seed from (`tui --new --upstream`). Repeatable.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[arg(long)]
    pub upstream: Vec<String>,
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
pub(crate) struct CreatedResult {
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

    fn execute(self, state: &mut Control, _context: &RequestContext) -> Result<CreatedResult> {
        let here = std::env::current_dir().context("reading the working directory")?;
        let cwd = match self.cwd {
            Some(cwd) => here.join(cwd),
            None => here,
        };
        let env = efference::env_table::EnvSnapshot::capture()
            .map_err(anyhow::Error::msg)?
            .into_entries();
        let request = efference::daemon::CreateSession {
            name: self.name,
            cwd,
            config: self.config,
            model: self.model,
            spotter_model: self.spotter_model,
            triage: None,
            temperature: self.temperature,
            effort: self.effort,
            upstream: self.upstream,
            autonomous: self.autonomous,
            reauth: Vec::new(),
            keep_scratch: false,
            env,
        };
        let host = state.host_client()?;
        let id = state.block_on(host.create_session(&request))?;
        Ok(CreatedResult { id })
    }
}
