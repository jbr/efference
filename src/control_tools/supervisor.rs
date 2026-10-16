use super::{Control, Executor};
use anyhow::{Result, anyhow};
use mcplease::{
    traits::{Tool, ToolMeta},
    types::{RequestContext, ToolAnnotations},
};
use serde::{Deserialize, Serialize};

/// Set which session supervises a session, or clear it. A session's supervisor
/// answers its gates when its review level is `supervisor` (a session at
/// `spotter_human` moves there when given one), may drive it with `turn`,
/// `approve`, and `interrupt`, and is woken by its status. By default it is the
/// session that created it; a session the operator created has none. Clearing
/// hands its gates to the operator, and a gate already held is re-routed rather
/// than dropped.
#[derive(Debug, Serialize, Deserialize, schemars::JsonSchema, clap::Args)]
#[serde(rename = "supervisor")]
pub struct Supervisor {
    /// The session to supervise it, by id (see `sessions`). Omitted with `clear`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub supervisor: Option<String>,
    /// Clear the supervisor instead of setting one.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    #[arg(long)]
    pub clear: bool,
    /// The session whose supervisor is being set, by id (see `sessions`).
    #[arg(long)]
    pub session: String,
}

impl ToolMeta for Supervisor {
    fn title() -> Option<&'static str> {
        Some("Set a session's supervisor")
    }

    fn annotations() -> Option<ToolAnnotations> {
        Some(ToolAnnotations {
            read_only_hint: Some(false),
            destructive_hint: Some(false),
            idempotent_hint: Some(true),
            open_world_hint: Some(false),
            ..Default::default()
        })
    }
}

impl Tool<Control> for Supervisor {
    type Output = String;

    async fn execute(self, control: Control, _context: RequestContext) -> Result<String> {
        let supervisor = match (self.supervisor, self.clear) {
            (Some(supervisor), false) => Some(supervisor),
            (None, true) => None,
            (Some(_), true) => return Err(anyhow!("pass a supervisor or `clear`, not both")),
            (None, false) => return Err(anyhow!("pass a supervisor, or `clear` to clear it")),
        };
        match &control.executor {
            Executor::Client(state) => {
                let host = state.host_client()?;
                let set = host
                    .set_supervisor(&self.session, supervisor.as_deref())
                    .await?;
                Ok(match set {
                    Some(supervisor) => format!("{}: supervised by {supervisor}", self.session),
                    None => format!("{}: no supervisor", self.session),
                })
            }
            // Not mounted in a session's toolbox: a session handing on a session it
            // supervises has no in-session verb (docs/tools.md).
            Executor::Host(_) => Err(anyhow!(
                "`supervisor` is an operator verb, not mounted in a session (docs/tools.md)"
            )),
        }
    }
}
