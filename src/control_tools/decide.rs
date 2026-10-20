use super::args::{Decision, handle, reader_name, turn_source};
use super::executor::{state_of, status_in};
use super::host_executor::names_held;
use super::status::{operator_relation, session_relation};
use super::{Control, Executor};
use crate::daemon::status::{Condition, HeldCall, Moves, Reader, StatusView, answers};
use crate::frontend::Approval;
use anyhow::{Result, anyhow};
use mcplease::{
    traits::{Tool, ToolMeta},
    types::{RequestContext, ToolAnnotations},
};
use serde::{Deserialize, Serialize};

/// Decide the call an agent is holding for a decision. `allow` runs it, and a
/// message rides with its result; `deny` refuses it — with a message the agent goes
/// on under it, and a bare deny ends its turn.
#[derive(Debug, Serialize, Deserialize, schemars::JsonSchema, clap::Args)]
#[serde(rename = "decide")]
pub struct Decide {
    /// The agent's id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[arg(long)]
    pub agent: Option<String>,
    /// The held call's seq, as the agent's status gives it (`8`, or `8.2` for one of
    /// several calls).
    #[serde(deserialize_with = "handle")]
    pub seq: String,
    /// `allow` or `deny`.
    #[arg(value_enum)]
    pub decision: Decision,
    /// With `allow`, a note the agent reads with the call's result; with `deny`, the
    /// message it goes on under.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[arg(long)]
    pub message: Option<String>,
}

impl ToolMeta for Decide {
    fn title() -> Option<&'static str> {
        Some("Decide a held call")
    }

    fn annotations() -> Option<ToolAnnotations> {
        Some(ToolAnnotations {
            read_only_hint: Some(false),
            // Allowing runs a call the rungs did not clear; what it then does is
            // the call's business, not this tool's.
            destructive_hint: Some(true),
            idempotent_hint: Some(false),
            open_world_hint: Some(false),
            ..Default::default()
        })
    }
}

impl Decide {
    /// The note, when it says anything.
    fn note(&self) -> Option<&str> {
        self.message
            .as_deref()
            .filter(|note| !note.trim().is_empty())
    }

    /// The decision the gate takes: allow approves; deny redirects under its message,
    /// or aborts the turn without one.
    fn approval(&self) -> Approval {
        match (self.decision, self.note()) {
            (Decision::Allow, _) => Approval::Approve,
            (Decision::Deny, Some(message)) => Approval::Redirect(message.to_string()),
            (Decision::Deny, None) => Approval::Abort,
        }
    }

    fn allow(&self) -> bool {
        self.decision == Decision::Allow
    }

    /// The submission id of the turn that will carry an allow's note, minted before
    /// the decision so the decision's record can name it.
    fn note_submission(&self) -> Option<String> {
        (self.allow() && self.note().is_some()).then(crate::daemon::mint_submission_id)
    }
}

/// The held call `view` shows when it is the one `seq` names.
fn held_named(view: &StatusView, seq: &str) -> Option<HeldCall> {
    match &view.condition {
        Condition::Gated(held) if names_held(held.handle.as_deref(), seq) => Some(held.clone()),
        _ => None,
    }
}

impl Tool<Control> for Decide {
    type Output = String;

    async fn execute(self, control: Control, context: RequestContext) -> Result<String> {
        let now = super::now();
        match &control.executor {
            Executor::Client(state) => {
                let client = state.client(self.agent.as_deref()).await?;
                let me = reader_name(&context);
                let moved = |view: &StatusView| {
                    let reader = Reader::read(operator_relation(view), &Moves::MCP, me.as_deref());
                    anyhow!(answers::gate_moved(&self.seq, view, &reader, now))
                };
                let view = status_in(&state_of(&client).await?)?;
                let Some(held) = held_named(&view, &self.seq) else {
                    return Err(moved(&view));
                };
                // The decider, as a turn's sender is: the mounting harness from its
                // clientInfo over MCP, nobody (the operator) from the command line.
                let source = turn_source(&context);
                let note_submission = self.note_submission();
                if !client
                    .decide(
                        &held.call_id,
                        &self.approval(),
                        source.as_deref(),
                        note_submission.as_deref(),
                    )
                    .await?
                {
                    return Err(moved(&status_in(&state_of(&client).await?)?));
                }
                if let (Some(note), Some(submission_id)) = (self.note(), note_submission)
                    && let Err(refusal) = client
                        .submit(
                            &crate::frontend::SubmittedTurn::new(note, submission_id)
                                .sourced(source.clone()),
                            None,
                        )
                        .await?
                {
                    return Err(anyhow!(
                        "{}; your note was not delivered: {refusal}",
                        answers::decided(&view.id, &held, true, None)
                    ));
                }
                Ok(answers::decided(&view.id, &held, self.allow(), self.note()))
            }
            Executor::Host(state) => {
                // Only the target's supervisor decides in-session (docs/permissions.md).
                let (target, id) = state.resolve_decided(self.agent.as_deref())?;
                let caller = state.caller();
                let moved = |view: &StatusView| {
                    let relation = session_relation(state, view, true);
                    let reader = Reader::read(relation, &Moves::IN_SESSION, Some(caller));
                    anyhow!(answers::gate_moved(&self.seq, view, &reader, now))
                };
                let view = target.status_view(&id, &target.status());
                let gate = target
                    .pending_gate()
                    .filter(|gate| names_held(gate.handle.as_deref(), &self.seq));
                let (Some(gate), Some(held)) = (gate, held_named(&view, &self.seq)) else {
                    return Err(moved(&view));
                };
                // Decided by the calling session, as its messages are sent (docs/tools.md).
                let source = crate::frontend::session_source(caller);
                let note_submission = self.note_submission();
                if !target.resolve_approval(
                    &gate.call.id,
                    self.approval(),
                    Some(source.clone()),
                    note_submission.clone(),
                ) {
                    return Err(moved(&target.status_view(&id, &target.status())));
                }
                if let (Some(note), Some(submission_id)) = (self.note(), note_submission) {
                    let turn = crate::frontend::SubmittedTurn::new(note, submission_id)
                        .sourced(Some(source));
                    if let Err(refusal) = target.submit_named_turn(turn, None) {
                        return Err(anyhow!(
                            "{}; your note was not delivered: {}",
                            answers::decided(&id, &held, true, None),
                            refusal.message(&id, Some(caller))
                        ));
                    }
                }
                Ok(answers::decided(&id, &held, self.allow(), self.note()))
            }
        }
    }
}
