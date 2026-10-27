use super::{Control, Executor};
use anyhow::Result;
use mcplease::{
    traits::{Tool, ToolMeta},
    types::{RequestContext, ToolAnnotations},
};
use serde::{Deserialize, Serialize};

/// Resolve a pending config-edit proposal — the spotter's prefix-promotion, filed
/// non-blocking and inert until an orchestrator-above decides it.
/// Ids and the proposed change come from `state.proposals`. With no `cancel`, the
/// proposal commits: its change folds into the effective config, and for a
/// session-visible key (`scope`, `paths`) the session is notified. `cancel` drops
/// it from the queue instead, audit-only, touching nothing.
///
/// A session can file a proposal but never resolve its own — the caller here is
/// the orchestrator-above, one level up from whoever's perimeter is changing.
// docs/permissions.md
#[derive(Debug, Serialize, Deserialize, schemars::JsonSchema, clap::Args)]
#[serde(rename = "proposal")]
pub struct Proposal {
    /// The proposal's id — the `Proposed` log entry's `seq`, from `state.proposals`.
    pub seq: u64,
    /// Drop the proposal instead of committing it. Audit-only: the queue empties,
    /// nothing else changes.
    #[serde(default)]
    #[arg(long)]
    pub cancel: bool,
    /// The rationale for the decision, addressed to the session and kept as the
    /// audit record of why the perimeter did or did not move.
    pub note: String,
    /// The agent's id. Defaults to the one session live here.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[arg(long)]
    pub agent: Option<String>,
}

impl ToolMeta for Proposal {
    fn title() -> Option<&'static str> {
        Some("Commit or cancel a proposal")
    }

    fn annotations() -> Option<ToolAnnotations> {
        Some(ToolAnnotations {
            read_only_hint: Some(false),
            // Committing folds a change into the effective config; what that then
            // authorizes is the classifier's business, not this tool's.
            destructive_hint: Some(true),
            idempotent_hint: Some(false),
            open_world_hint: Some(false),
            ..Default::default()
        })
    }
}

impl Tool<Control> for Proposal {
    type Output = String;

    async fn execute(self, control: Control, _context: RequestContext) -> Result<String> {
        let verb = if self.cancel {
            "cancelled"
        } else {
            "committed"
        };
        match &control.executor {
            Executor::Client(state) => {
                let client = state.client(self.agent.as_deref()).await?;
                if self.cancel {
                    client.cancel_proposal(self.seq, &self.note).await?;
                } else {
                    client.commit_proposal(self.seq, &self.note).await?;
                }
                Ok(format!("{verb} {}", self.seq))
            }
            Executor::Host(_) => Err(super::operator_verb("proposal")),
        }
    }
}
