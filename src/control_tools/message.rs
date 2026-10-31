use super::args::{NamedAgent, optional_handle, turn_source};
use super::executor::{state_of, status_in};
use super::verb::{ControlVerb, VerbCall};
use super::{ClientExecutor, HostExecutor};
use crate::daemon::status::answers;
use crate::log::Notification;
use crate::protocol::turns::mint_submission_id;
use anyhow::{Result, anyhow};
use mcplease::{traits::ToolMeta, types::ToolAnnotations};
use serde::{Deserialize, Serialize};

/// Send an agent a message. From a session that drives it — the one that created
/// it, its supervisor, or the operator — the message is its next user message, and
/// an empty one continues it; from anyone else it is a note the agent may set aside.
/// The answer says which happened.
#[derive(Debug, Serialize, Deserialize, schemars::JsonSchema, clap::Args)]
#[serde(rename = "message")]
pub struct Message {
    #[command(flatten)]
    pub agent: NamedAgent,
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

impl ControlVerb for Message {
    type Output = String;

    async fn client(self, state: &ClientExecutor, call: &VerbCall<'_>) -> Result<String> {
        let text = self.text();
        let blank = text.trim().is_empty();
        let now = super::now();
        // An operator client drives every session (docs/tools.md).
        let client = state.client(Some(self.agent.id())).await?;
        let view = status_in(&state_of(&client).await?).ok();
        let id = view
            .as_ref()
            .map_or_else(|| self.agent.id().to_string(), |view| view.id.clone());
        let source = turn_source(call.context);
        if blank && self.reply_to.is_none() {
            return match client.continue_loop(source.as_deref()).await? {
                crate::client::ContinueOutcome::Accepted => Ok(answers::continuing(&id)),
                crate::client::ContinueOutcome::Declined { message } => {
                    Err(anyhow!("not continued: {message}"))
                }
            };
        }
        let turn = crate::frontend::SubmittedTurn::new(text, mint_submission_id()).sourced(source);
        let sent = client.submit(&turn, self.reply_to.as_deref()).await?;
        match sent {
            Ok(landing) => Ok(answers::sent(&id, landing, view.as_ref(), now)),
            Err(refusal) => Err(anyhow!(refusal)),
        }
    }

    async fn host(self, state: &HostExecutor, _call: &VerbCall<'_>) -> Result<String> {
        let text = self.text();
        let blank = text.trim().is_empty();
        let now = super::now();
        let (target, id) = state.resolve(Some(self.agent.id()))?;
        if !state.drives(&id)? {
            if self.reply_to.is_some() {
                return Err(anyhow!(answers::reply_from_a_peer(&id)));
            }
            if blank {
                return Err(anyhow!(answers::empty_from_a_peer(&id)));
            }
            // A note from a session that does not drive the target, under the caller's
            // id, and its project when that is another (docs/inbox.md).
            target.notify(Notification::new(
                state.note_source_for(&target),
                text,
                None,
            ));
            return Ok(answers::left_in_inbox(&id));
        }
        if blank && self.reply_to.is_none() {
            let origin =
                crate::log::ContinueOrigin::default().with_source(Some(state.source_for(&target)));
            return match target.request_continue(origin) {
                Ok(()) => Ok(answers::continuing(&id)),
                Err(why) => Err(anyhow!("not continued: {}", why.message())),
            };
        }
        let view = target.status_view(&id, &target.status());
        let source = state.source_for(&target);
        let turn = crate::frontend::SubmittedTurn::new(text, mint_submission_id())
            .sourced(Some(source.clone()));
        match target.submit_named_turn(turn, self.reply_to.as_deref()) {
            Ok(landing) => Ok(answers::sent(&id, landing, Some(&view), now)),
            Err(refusal) => Err(anyhow!(refusal.message(&id, Some(state.caller())))),
        }
    }
}
