use super::{Control, Executor};
use anyhow::Result;
use mcplease::{
    traits::{Tool, ToolMeta},
    types::{RequestContext, ToolAnnotations},
};
use serde::{Deserialize, Serialize};

/// File an ambient message in the session's inbox. Unlike `turn`, it does not
/// interrupt: it waits for the session's next triage point and is delivered there.
/// This is the right call for anything the session should know but need not stop
/// for — a build result, a review comment, a change upstream.
#[derive(Debug, Serialize, Deserialize, schemars::JsonSchema, clap::Args)]
#[serde(rename = "notify")]
pub struct Notify {
    /// Where it came from, shown with the message — e.g. `test-watch`, `operator`,
    /// `agent:reviewer`.
    pub source: String,
    /// The notification text. Fragments are joined with spaces, so a single-element
    /// array is the ordinary call.
    #[arg(required = true, num_args = 1..)]
    pub body: Vec<String>,
    /// Bind to a logical slot: a later notification on the same slot supersedes this
    /// one instead of piling up beside it.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[arg(long)]
    pub slot: Option<String>,
    /// Supersede one specific pending notification, by its id.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[arg(long)]
    pub replaces: Option<u64>,
    /// Notify this session by id (see `sessions`). Required unless the server was started with `--name`/`--socket`; then it overrides that session for this call.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[arg(long)]
    pub session: Option<String>,
}

impl ToolMeta for Notify {
    fn title() -> Option<&'static str> {
        Some("Notify the session")
    }

    fn annotations() -> Option<ToolAnnotations> {
        Some(ToolAnnotations {
            read_only_hint: Some(false),
            destructive_hint: Some(false),
            idempotent_hint: Some(false),
            open_world_hint: Some(false),
            ..Default::default()
        })
    }
}

impl Tool<Control> for Notify {
    type Output = String;

    async fn execute(self, control: Control, _context: RequestContext) -> Result<String> {
        match &control.executor {
            Executor::Client(state) => {
                let client = state.client(self.session.as_deref()).await?;
                let body = self.body.join(" ");
                let id = client
                    .notify(&self.source, &body, self.slot.as_deref(), self.replaces)
                    .await?;
                Ok(format!("notified (id {id})"))
            }
            Executor::Host(state) => {
                // `notify` is an ambient message, not a mutation of the target:
                // it reaches anything in the caller's project (docs/tools.md).
                let session = state.resolve(self.session.as_deref())?;
                let body = self.body.join(" ");
                let id =
                    session.notify(self.source.clone(), body, self.slot.clone(), self.replaces);
                Ok(format!("notified (id {id})"))
            }
        }
    }
}
