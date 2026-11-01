//! A session between its reservation and its adoption, as the `starting` topic
//! publishes it (docs/control-api.md). The host's entry for it is `host::starting`.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::protocol::turns::QueuedTurn;

/// The `starting` topic: a session reserved on the host whose environment and MCP
/// servers are being prepared, or whose preparation failed. `null` once the session has
/// started, which is every value a running session publishes.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
pub struct StartingView {
    pub id: String,
    /// The project root holding the session, which a resume names.
    pub cwd: PathBuf,
    /// When preparation began, absolute Unix millis.
    pub since: u64,
    /// Reopening a dormant session rather than creating one.
    pub resume: bool,
    pub stage: Stage,
    /// The MCP server whose authorization preparation waits on; `null` when none.
    pub authorizing: Option<String>,
    /// Why preparation ended without a session; `null` while it runs, and after an
    /// archive stopped it.
    pub failure: Option<StartFailure>,
}

/// Why preparation ended without a session, and the turns sent while it was starting,
/// handed back for a client to put in a composer.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
pub struct StartFailure {
    pub reason: String,
    /// When it failed, absolute Unix millis.
    pub at: u64,
    /// The turns queued while starting, in submission order.
    pub withdrawn: Vec<QueuedTurn>,
}

/// Which part of preparation is running (docs/control-api.md).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema), schemars(rename = "StartingStage"))]
#[serde(rename_all = "snake_case")]
pub enum Stage {
    /// Establishing the session's environment: the user-wide `env_hook` (docs/env.md).
    #[default]
    Environment,
    /// `boot::prepare` or `boot::resume`: the config, the provider lookup, the MCP
    /// mounts, the preamble.
    Preparing,
}
