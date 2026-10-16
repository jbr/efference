use super::{Control, Executor, pretty};
use anyhow::Result;
use mcplease::{
    traits::{Tool, ToolMeta, ToolOutput},
    types::{RequestContext, ToolAnnotations},
};
use serde::{Deserialize, Serialize};

/// Hand this session's work to a new session in the same directory, which
/// starts fresh from the project's current config — its grants, its preamble,
/// its MCP servers — and takes this session's place: the operator's clients
/// move to it, and it may archive this one once it has no more questions. The
/// brief is its first message, shown as this session's hand-off. Always put to
/// the operator before it runs; say so before calling it.
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

/// A hand-off as a tool result: the successor's id.
#[derive(Serialize, schemars::JsonSchema)]
pub struct HandedOff {
    successor: String,
}

impl ToolOutput for HandedOff {
    fn output_schema() -> Option<serde_json::Value> {
        Some(mcplease::traits::schema_for::<Self>().into())
    }

    fn to_text(&self) -> String {
        pretty(self)
    }
}

impl Tool<Control> for HandOff {
    type Output = HandedOff;

    async fn execute(self, control: Control, _context: RequestContext) -> Result<HandedOff> {
        match &control.executor {
            Executor::Client(_) => anyhow::bail!(
                "`hand_off` is a session handing off its own work; from outside a session, \
                 `create` starts one (docs/tools.md)"
            ),
            Executor::Host(state) => {
                // The successor starts where the caller works, with the caller's
                // child environment as its base — the same as `create` — and
                // resolves the project's config afresh, which is the point.
                let caller = state.caller_session()?;
                let request = crate::daemon::CreateSession {
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
                Ok(HandedOff { successor })
            }
        }
    }
}
