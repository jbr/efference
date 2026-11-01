use super::verb::{ControlVerb, VerbCall};
use super::{ClientExecutor, HostExecutor};
use anyhow::Result;
use mcplease::{traits::ToolMeta, types::ToolAnnotations};
use serde::{Deserialize, Serialize};

/// Hand your work to a new session in this directory, which starts from the
/// project's current config and takes your place: the operator's clients move to
/// it, and it archives you once it has no more questions. The brief is its first
/// message. Always put to the operator; say so before calling it.
#[derive(Debug, Serialize, Deserialize, schemars::JsonSchema)]
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

impl ControlVerb for HandOff {
    type Output = String;

    async fn client(self, _state: &ClientExecutor, _call: &VerbCall<'_>) -> Result<String> {
        anyhow::bail!(
            "`hand_off` is a session handing off its own work; from outside a session, \
             `create` starts one"
        )
    }

    async fn host(self, state: &HostExecutor, _call: &VerbCall<'_>) -> Result<String> {
        // The successor starts where the caller works, with the caller's child
        // environment as its base — the same as `create` — and resolves the project's
        // config afresh, which is the point.
        let caller = state.caller_session()?;
        // The host substitutes the caller's own options and sets `detached` from the
        // caller's own placement.
        let request = crate::protocol::sessions::CreateSession {
            env: Some(caller.child_env().resolved()),
            ..crate::protocol::sessions::CreateSession::new(caller.cwd().to_path_buf())
        };
        let successor = state.hand_off(request, self.brief).await?;
        Ok(format!(
            "handed off to {successor}: the operator's clients follow it there, and it \
             archives you once it has no more questions"
        ))
    }
}
