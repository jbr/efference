use super::{Control, Executor};
use anyhow::Result;
use mcplease::{
    traits::{Tool, ToolMeta},
    types::{RequestContext, ToolAnnotations},
};
use serde::{Deserialize, Serialize};

/// Hand your work to a new session in this directory, which starts from the
/// project's current config and takes your place: the operator's clients move to
/// it, and it archives you once it has no more questions. The brief is its first
/// message. Always put to the operator; say so before calling it.
#[derive(Debug, Serialize, Deserialize, schemars::JsonSchema, clap::Args)]
#[serde(rename = "hand_off")]
pub struct HandOff {
    /// What the new session needs to start: what the operator wants done, what
    /// was decided, and anything it should ask this session about before
    /// archiving it. Written for the new session, not the operator.
    pub brief: String,
}

impl ToolMeta for HandOff {
    fn title() -> Option<&'static str> {
        Some("Hand off to a new session")
    }

    fn annotations() -> Option<ToolAnnotations> {
        Some(ToolAnnotations {
            read_only_hint: Some(false),
            destructive_hint: Some(false),
            // A second hand-off is a second successor.
            idempotent_hint: Some(false),
            open_world_hint: Some(false),
            ..Default::default()
        })
    }
}

impl Tool<Control> for HandOff {
    type Output = String;

    async fn execute(self, control: Control, _context: RequestContext) -> Result<String> {
        match &control.executor {
            Executor::Client(_) => anyhow::bail!(
                "`hand_off` is a session handing off its own work; from outside a session, \
                 `create` starts one"
            ),
            Executor::Host(state) => {
                // The successor starts where the caller works, with the caller's
                // child environment as its base — the same as `create` — and
                // resolves the project's config afresh, which is the point.
                let caller = state.caller_session()?;
                let request = crate::protocol::sessions::CreateSession {
                    name: None,
                    cwd: caller.cwd().to_path_buf(),
                    config: None,
                    // The host substitutes the caller's own options.
                    options: crate::inference::SessionOptions::default(),
                    upstream: Vec::new(),
                    autonomous: false,
                    reauth: Vec::new(),
                    refuse_authorization: false,
                    keep_scratch: false,
                    worktree: false,
                    // The host sets this from the caller's own placement.
                    detached: false,
                    env: Some(caller.child_env().resolved()),
                };
                let successor = state.hand_off(request, self.brief).await?;
                Ok(format!(
                    "handed off to {successor}: the operator's clients follow it there, and it \
                     archives you once it has no more questions"
                ))
            }
        }
    }
}
