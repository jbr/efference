use super::subscribe::no_inbox;
use super::{Control, Executor};
use anyhow::Result;
use mcplease::{
    traits::{Tool, ToolMeta},
    types::{RequestContext, ToolAnnotations},
};
use serde::{Deserialize, Serialize};

/// Stop hearing a session's status. A session you created stays subscribed
/// with `wake` until you unsubscribe it here.
#[derive(Debug, Serialize, Deserialize, schemars::JsonSchema, clap::Args)]
#[serde(rename = "unsubscribe")]
pub struct Unsubscribe {
    /// The session to stop hearing from, by id.
    pub session: String,
}

impl ToolMeta for Unsubscribe {
    fn title() -> Option<&'static str> {
        Some("Unsubscribe from a session's status")
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

impl Tool<Control> for Unsubscribe {
    type Output = String;

    fn execute(self, control: &mut Control, _context: &RequestContext) -> Result<String> {
        match &mut control.executor {
            Executor::Client(_) => Err(no_inbox()),
            Executor::Host(state) => {
                state.resolve(Some(&self.session))?;
                if state.unsubscribe(&self.session)? {
                    Ok(format!("unsubscribed from {}", self.session))
                } else {
                    Ok(format!("you were not subscribed to {}", self.session))
                }
            }
        }
    }
}

