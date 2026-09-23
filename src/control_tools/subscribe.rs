use super::{Control, Executor};
use anyhow::{Result, anyhow};
use mcplease::{
    traits::{Tool, ToolMeta},
    types::{RequestContext, ToolAnnotations},
};
use serde::{Deserialize, Serialize};

/// Hear a session's status in your inbox: each time it stops at a gate,
/// finishes a turn, stalls, or ends, a notification names the condition and
/// the call that answers it. A session you created is subscribed already, and
/// its statuses wake you; a subscription made here is ambient — triage may
/// dismiss it. Subscribing again replaces nothing and changes nothing.
#[derive(Debug, Serialize, Deserialize, schemars::JsonSchema, clap::Args)]
#[serde(rename = "subscribe")]
pub struct Subscribe {
    /// The session to hear from, by id (see `sessions`).
    pub session: String,
}

impl ToolMeta for Subscribe {
    fn title() -> Option<&'static str> {
        Some("Subscribe to a session's status")
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

impl Tool<Control> for Subscribe {
    type Output = String;

    async fn execute(self, control: Control, _context: RequestContext) -> Result<String> {
        match &control.executor {
            Executor::Client(_) => Err(no_inbox()),
            Executor::Host(state) => {
                // Observation, not mutation: anything in the caller's project.
                state.resolve(Some(&self.session))?;
                if state.subscribe(&self.session, false)? {
                    Ok(format!(
                        "subscribed to {}; its status will arrive in your inbox",
                        self.session
                    ))
                } else {
                    Err(anyhow!("no session {:?} to subscribe to", self.session))
                }
            }
        }
    }
}

/// A subscription delivers to a session's inbox, which an external client does
/// not have (docs/tools.md).
pub(super) fn no_inbox() -> anyhow::Error {
    anyhow!(
        "subscriptions deliver to a session's inbox; from outside a session, wait with \
         `settle` or the `wait` flag on `turn`/`approve` instead"
    )
}
