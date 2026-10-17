use super::args::{optional_handle, turn_source};
use super::executor::{state_of, status_in};
use super::{Control, Executor};
use crate::daemon::status::answers;
use anyhow::{Result, anyhow};
use mcplease::{
    traits::{Tool, ToolMeta},
    types::{RequestContext, ToolAnnotations},
};
use serde::{Deserialize, Serialize};

/// Send an agent a message. From a session that drives it — the one that created
/// it, its supervisor, or the operator — the message is its next user message, and
/// an empty one continues it; from anyone else it is a note the agent may set aside.
/// The answer says which happened.
#[derive(Debug, Serialize, Deserialize, schemars::JsonSchema, clap::Args)]
#[serde(rename = "message")]
pub struct Message {
    /// The agent's id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[arg(long)]
    pub agent: Option<String>,
    /// The message. Empty continues an agent you drive.
    #[arg(skip)]
    pub text: String,
    /// The message's words, joined with spaces. Command-line only; an MCP or
    /// in-session call sends `text`.
    #[serde(skip)]
    #[schemars(skip)]
    #[arg(num_args = 0..)]
    pub words: Vec<String>,
    /// The seq of the question this answers, as the agent's status gives it.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "optional_handle"
    )]
    #[arg(long)]
    pub reply_to: Option<String>,
}

impl ToolMeta for Message {
    fn title() -> Option<&'static str> {
        Some("Message an agent")
    }

    fn annotations() -> Option<ToolAnnotations> {
        Some(ToolAnnotations {
            read_only_hint: Some(false),
            destructive_hint: Some(false),
            // Two identical messages are two messages.
            idempotent_hint: Some(false),
            open_world_hint: Some(false),
            ..Default::default()
        })
    }
}

impl Message {
    /// The message as sent: the command line's words, else `text`.
    fn text(&self) -> String {
        match self.words.is_empty() {
            true => self.text.clone(),
            false => self.words.join(" "),
        }
    }
}

impl Tool<Control> for Message {
    type Output = String;

    async fn execute(self, control: Control, context: RequestContext) -> Result<String> {
        let text = self.text();
        let blank = text.trim().is_empty();
        let now = super::now();
        match &control.executor {
            Executor::Client(state) => {
                // An operator client drives every session (docs/tools.md).
                let client = state.client(self.agent.as_deref()).await?;
                let view = status_in(&state_of(&client).await?).ok();
                let id = view
                    .as_ref()
                    .map(|view| view.id.clone())
                    .or(self.agent.clone())
                    .unwrap_or_else(|| "the session".to_string());
                if blank && self.reply_to.is_none() {
                    return match client.continue_loop().await? {
                        crate::client::ContinueOutcome::Accepted => Ok(answers::continuing(&id)),
                        crate::client::ContinueOutcome::Declined { message } => {
                            Err(anyhow!("not continued: {message}"))
                        }
                    };
                }
                let source = turn_source(&context);
                let sent = client
                    .submit_named(&text, source.as_deref(), self.reply_to.as_deref())
                    .await?;
                match sent {
                    Ok(landing) => Ok(answers::sent(&id, landing, view.as_ref(), now)),
                    Err(refusal) => Err(anyhow!(refusal)),
                }
            }
            Executor::Host(state) => {
                let (target, id) = state.resolve(self.agent.as_deref())?;
                if !state.drives(&id)? {
                    if self.reply_to.is_some() {
                        return Err(anyhow!(answers::reply_from_a_peer(&id)));
                    }
                    if blank {
                        return Err(anyhow!(answers::empty_from_a_peer(&id)));
                    }
                    // A note from a session that does not drive the target, under the
                    // caller's id (docs/inbox.md).
                    target.notify(state.caller().to_string(), text, None);
                    return Ok(answers::left_in_inbox(&id));
                }
                if blank && self.reply_to.is_none() {
                    return match target.request_continue() {
                        Ok(()) => Ok(answers::continuing(&id)),
                        Err(why) => Err(anyhow!("not continued: {}", why.message())),
                    };
                }
                let view = target.status_view(&id, &target.status());
                let source = crate::frontend::session_source(state.caller());
                let turn =
                    crate::frontend::SubmittedTurn::new(text, crate::daemon::mint_submission_id())
                        .sourced(Some(source.clone()));
                match target.submit_named_turn(turn, self.reply_to.as_deref()) {
                    Ok(landing) => Ok(answers::sent(&id, landing, Some(&view), now)),
                    Err(refusal) => Err(anyhow!(refusal.message(&id, Some(state.caller())))),
                }
            }
        }
    }
}
