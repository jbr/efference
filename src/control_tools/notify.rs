use super::{Control, Executor};
use anyhow::Result;
use mcplease::{
    traits::{Tool, ToolMeta},
    types::{RequestContext, ToolAnnotations},
};
use serde::{Deserialize, Serialize};

/// File an ambient message in a session's inbox under a label of your choosing. It
/// does not interrupt: it waits for the session's next triage point and is delivered
/// there — a build result, a review comment, a change upstream. The operator's verb
/// for scripts; an agent's note is `message` from a session that does not drive its
/// target.
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
    /// The agent's id. Defaults to the session `--name` names, or the one live here.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[arg(long)]
    pub agent: Option<String>,
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

/// What `notify` answers: the arrival entry's seq, the handle a triage addresses it by.
/// A slot notification has no entry until a triage shows it.
fn answer(seq: Option<u64>) -> String {
    match seq {
        Some(seq) => format!("notified (seq {seq})"),
        None => "notified".to_string(),
    }
}

impl Tool<Control> for Notify {
    type Output = String;

    async fn execute(self, control: Control, _context: RequestContext) -> Result<String> {
        match &control.executor {
            Executor::Client(state) => {
                let client = state.client(self.agent.as_deref()).await?;
                let body = self.body.join(" ");
                let seq = client
                    .notify(&self.source, &body, self.slot.as_deref())
                    .await?;
                Ok(answer(seq))
            }
            Executor::Host(_) => Err(super::operator_verb("notify")),
        }
    }
}
