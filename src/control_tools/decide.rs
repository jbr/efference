use super::args::{Decision, handle, reader_name, turn_source};
use super::executor::{state_of, status_in};
use super::host_executor::names_held;
use super::status::{operator_relation, session_relation};
use super::{Control, Executor};
use crate::daemon::status::{Condition, HeldCall, Moves, Reader, Reviewer, StatusView, answers};
use crate::frontend::Approval;
use anyhow::{Result, anyhow};
use mcplease::{
    traits::{Tool, ToolMeta},
    types::{RequestContext, ToolAnnotations},
};
use serde::{Deserialize, Serialize};

/// Decide the call an agent is holding for a decision. `allow` runs it, and a
/// message rides with its result; `deny` refuses it — with a message the agent goes
/// on under it, and a bare deny ends its turn; `escalate` leaves it held and passes it
/// to whoever decides above you, with a message as a note to them.
#[derive(Debug, Serialize, Deserialize, schemars::JsonSchema, clap::Args)]
#[serde(rename = "decide")]
pub struct Decide {
    /// The agent's id.
    #[arg(long)]
    pub agent: String,
    /// The held call's seq, as the agent's status gives it (`8`, or `8.2` for one of
    /// several calls).
    #[serde(deserialize_with = "handle")]
    pub seq: String,
    /// `allow`, `deny`, or `escalate`.
    #[arg(value_enum)]
    pub decision: Decision,
    /// With `allow`, a note the agent reads with the call's result; with `deny`, the
    /// message it goes on under; with `escalate`, a note to whoever decides next.
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
    /// or aborts the turn without one. `None` for an escalation, which resolves nothing.
    fn approval(&self) -> Option<Approval> {
        match (self.decision, self.note()) {
            (Decision::Allow, _) => Some(Approval::Approve),
            (Decision::Deny, Some(message)) => Some(Approval::Redirect(message.to_string())),
            (Decision::Deny, None) => Some(Approval::Abort),
            (Decision::Escalate, _) => None,
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
                let client = state.client(Some(&self.agent)).await?;
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
                let Some(approval) = self.approval() else {
                    // An operator client is on the operator's side, so its escalation
                    // asks the person: it goes to the operator (docs/permissions.md).
                    if !client
                        .escalate(&held.call_id, source.as_deref(), self.note())
                        .await?
                    {
                        return Err(moved(&status_in(&state_of(&client).await?)?));
                    }
                    return Ok(answers::escalated(&view.id, &held, None, self.note()));
                };
                let note_submission = self.note_submission();
                if !client
                    .decide(
                        &held.call_id,
                        &approval,
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
                // Only the target's supervisor, or the session the held call was
                // escalated to, decides in-session (docs/permissions.md).
                let (target, id) = state.resolve_decided(Some(&self.agent), &self.seq)?;
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
                let source = state.source_for(&target);
                let Some(approval) = self.approval() else {
                    return escalate(
                        state,
                        &target,
                        &id,
                        &held,
                        &gate.call.id,
                        source,
                        self.note(),
                    );
                };
                let note_submission = self.note_submission();
                if !target.resolve_approval(
                    &gate.call.id,
                    approval,
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

/// An in-session escalation (docs/permissions.md): only the session a held call is
/// routed to passes it on, to its own supervisor while that one is hosted, else to the
/// operator.
fn escalate(
    state: &super::HostExecutor,
    target: &crate::daemon::Session,
    id: &str,
    held: &HeldCall,
    call_id: &crate::log::CallId,
    source: String,
    note: Option<&str>,
) -> Result<String> {
    let caller = state.caller();
    match &held.reviewer {
        Reviewer::Supervisor { id: reviewer } if reviewer == caller => {}
        reviewer => return Err(anyhow!(answers::not_yours_to_escalate(id, held, reviewer))),
    }
    // The next supervisor up. The holder never decides its own gate, so a chain that
    // led back to it would go to the operator instead.
    let next = state
        .caller_session()?
        .supervisor()
        .filter(|next| next != id);
    if !target.escalate(
        call_id,
        Some(source),
        next.clone(),
        note.map(str::to_string),
    ) {
        let view = target.status_view(id, &target.status());
        let reader = Reader::read(
            session_relation(state, &view, state.drives(id)?),
            &Moves::IN_SESSION,
            Some(caller),
        );
        return Err(anyhow!(answers::gate_moved(
            held.handle.as_deref().unwrap_or_default(),
            &view,
            &reader,
            super::now()
        )));
    }
    Ok(answers::escalated(id, held, next.as_deref(), note))
}
