//! In-flight tool-call approvals awaiting a decision (docs/permissions.md).

use super::*;
use crate::protocol::state::Gate;
use crate::protocol::status::Escalated;

/// The gate held now: the [`Gate`] the `gate` topic publishes, and whom its latest push
/// moment went to.
#[derive(Clone)]
pub(super) struct Pending {
    pub(super) gate: Gate,
    /// Whom the gate's latest push moment went to: the session that answers it, or
    /// `None` for the operator (docs/web-push.md). A routing change that moves the
    /// gate's reviewer away from this is a moment of its own (`Session::gate_rerouted`).
    pub(super) moment_to: Option<String>,
}

impl Pending {
    /// What a push about this gate says (docs/web-push.md): the gate's own body, marked
    /// with its last escalation's escalator and note when it was escalated.
    pub(super) fn push_body(&self) -> String {
        let held = &self.gate;
        let call = ToolCall::new(&held.name, held.call_id.as_str(), held.input.clone());
        let gate = push::gate_body(
            &held.name,
            call.intent(),
            held.reason.as_deref(),
            held.acting_on.as_ref(),
        );
        match held.escalations.last() {
            Some(last) => push::escalated_body(last.source.as_deref(), last.note.as_deref(), &gate),
            None => gate,
        }
    }
}

/// Who is deciding a held call, and the turn that carries their note: the optional inputs
/// of [`Session::resolve_approval`] (docs/permissions.md). The default is the operator's own
/// command line or TUI, deciding without a note.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Decider {
    /// The decider, spelled as a turn's `source` is (`None` for the operator).
    pub(crate) source: Option<String>,
    /// The submission id of the turn carrying an allow's note, which the decider submits
    /// after the decision lands (`decide`, docs/tools.md).
    pub(crate) note_submission: Option<String>,
}

impl Decider {
    /// This decision, sent by `source`.
    pub(crate) fn with_source(mut self, source: Option<String>) -> Self {
        self.source = source;
        self
    }

    /// This decision, with its note riding the turn `submission_id`.
    pub(crate) fn with_note_submission(mut self, submission_id: Option<String>) -> Self {
        self.note_submission = submission_id;
        self
    }
}

/// A session's held call, as its parent's `agents__decide` reads it
/// (docs/permissions.md): the call the model wrote, its transcript handle, and the
/// directories its action is classified against.
pub(crate) struct PendingGate {
    pub(crate) call: ToolCall,
    /// The handle a decision names it by (`seq` or `seq.n`).
    pub(crate) handle: Option<String>,
    pub(crate) directories: crate::config::policy::Directories,
}

/// The `source` the loop's own permission review decides under, at the review level in
/// force: `review[<level>]` (docs/permissions.md). It writes the redirects no person
/// or agent wrote (the autonomous redirect, a `[commands]` deny, a config edit's
/// outcome), so a reader is not told the operator wrote them.
/// [`is_review_source`](crate::record::names::sources::is_review_source) recognizes the label.
fn review_source(level: crate::config::policy::ReviewLevel) -> String {
    format!("review[{}]", level.as_str())
}

impl Session {
    /// The spotter's reason for the most recent verdict on `call_id`, read from the log's `Spot`
    /// audit entries (docs/permissions.md). The spotter logs its verdict *before* routing the call
    /// to the human gate, so by the time the gate registers, the reason is already durably in the
    /// log — no side channel needed. The log is the source of truth, here too.
    pub(super) fn latest_spot_reason(&self, call_id: &CallId) -> Option<String> {
        self.latest_spot(call_id).map(|spot| spot.reason)
    }

    /// The most recent `Spot` entry on `call_id`: the verdict whose reason the gate
    /// shows, and whose recorded answers the status phrases (`status/view.rs`).
    pub(super) fn latest_spot(&self, call_id: &CallId) -> Option<crate::record::log::Spot> {
        self.log
            .lock()
            .unwrap()
            .entries()
            .iter()
            .rev()
            .find_map(|e| match &e.payload {
                Payload::Spot(s) if &s.call_id == call_id => Some(s.clone()),
                _ => None,
            })
    }

    /// The held call, as a parent's `agents__decide` reads it (docs/permissions.md):
    /// the call the model wrote, its handle, and the directories its action is
    /// classified against. `None` when no gate is open.
    pub(crate) fn pending_gate(&self) -> Option<PendingGate> {
        let call = {
            let pending = self.pending.lock().unwrap();
            let pending = pending.as_ref()?;
            ToolCall::new(
                pending.gate.name.clone(),
                pending.gate.call_id.clone(),
                pending.gate.input.clone(),
            )
        };
        Some(PendingGate {
            handle: self.handle_of(&call.id.0),
            call,
            directories: self.directories(),
        })
    }

    /// Register an approval gate and surface it to `GET /state`. Returns the
    /// receiver the loop awaits. `gate` carries the leaf action an `agents__decide`
    /// authorizes and the grant the gate offers (docs/permissions.md).
    pub(super) fn register_pending(
        &self,
        call: &ToolCall,
        gate: GateOptions<'_>,
    ) -> oneshot::Receiver<Approval> {
        let (tx, rx) = oneshot::channel();
        self.approvals.lock().unwrap().insert(call.id.clone(), tx);
        let reason = self.latest_spot_reason(&call.id);
        // Who answers the gate, read before it is stored: the status watcher compares
        // the gate's routing against this, so a gate that has not moved is no new moment.
        let reviewer = self.gate_reviewer(&[]);
        let pending = Pending {
            gate: Gate {
                call_id: call.id.0.clone(),
                name: call.name.clone(),
                input: call.input.clone(),
                reason,
                acting_on: gate.bound.cloned(),
                grant: gate.grant,
                escalations: Vec::new(),
            },
            moment_to: reviewer.clone(),
        };
        let push_body = pending.push_body();
        *self.pending.lock().unwrap() = Some(pending);
        // A gate is a settled state: the phase wakes any `/settle` waiter (the `?wait` on
        // `/turn` returns here). Moved after the gate is stored, so a waiter it wakes reads it.
        self.set_phase(LoopPhase::Gated);
        // …and a moment worth a push when the operator is its reviewer (docs/web-push.md).
        // Spawned, so the loop reaches its `rx.await` without waiting on a push service.
        self.moment(crate::record::log::MomentKind::Gate, reviewer, push_body);
        rx
    }

    /// Pass the held call `id` up undecided (docs/permissions.md): it stays held, routed
    /// now to the session `to`, or to the operator when `to` is `None`, with `note` for
    /// whoever decides next. `source` is the escalator, spelled as a turn's `source` is.
    /// `false` when `id` is not the pending gate.
    ///
    /// The escalation is recorded as a `Telemetry::Escalation` and carried on the gate
    /// topic and the status view; the status watcher (`subscriptions.rs`) re-posts the
    /// gate to its new reviewer. Each escalation is a gate moment (docs/web-push.md):
    /// one that reaches the operator is pushed, marked with who escalated it and the
    /// note; one that reaches a session is recorded as not sent.
    pub(crate) fn escalate(
        &self,
        id: &CallId,
        source: Option<String>,
        to: Option<String>,
        note: Option<String>,
    ) -> bool {
        let push_body = {
            let mut pending = self.pending.lock().unwrap();
            let Some(pending) = pending
                .as_mut()
                .filter(|pending| pending.gate.call_id == id.0)
            else {
                return false;
            };
            pending.gate.escalations.push(Escalated {
                source: source.clone(),
                to: to.clone(),
                note: note.clone(),
            });
            // Under the same lock as the escalation, so the status watcher never reads
            // the new routing as a moment this escalation has not announced.
            pending.moment_to = to.clone();
            pending.push_body()
        };
        let record = crate::record::log::Escalation::new(id.clone(), source, to.clone(), note);
        // The record must not stop what it describes.
        if let Err(e) = self.append(
            self.latest_turn(),
            Payload::Telemetry(crate::record::log::Telemetry::Escalation(record)),
        ) {
            log::warn!("escalation record: append failed: {e}");
        }
        self.moment(crate::record::log::MomentKind::Gate, to, push_body);
        // The gate topic and the status watchers read the new routing now.
        self.touch_state();
        true
    }

    /// The session the pending gate was last escalated to, with the gate's handle:
    /// `None` when no gate is open, or it was not escalated or went to the operator.
    /// The one gate that session may decide (docs/permissions.md).
    pub(crate) fn escalated_to(&self) -> Option<(String, Option<String>)> {
        let (call_id, to) = {
            let pending = self.pending.lock().unwrap();
            let pending = pending.as_ref()?;
            (
                pending.gate.call_id.clone(),
                pending.gate.escalations.last()?.to.clone()?,
            )
        };
        Some((to, self.handle_of(&call_id)))
    }

    /// Who decided the held call `id`, as its latest `Telemetry::GateDecision` names them:
    /// `None` for the operator's own command line or TUI, and for a call nobody decided.
    pub(crate) fn gate_decider(&self, id: &CallId) -> Option<String> {
        self.log
            .lock()
            .unwrap()
            .entries()
            .iter()
            .rev()
            .find_map(|entry| match &entry.payload {
                Payload::Telemetry(crate::record::log::Telemetry::GateDecision(decision))
                    if decision.call_id == *id =>
                {
                    Some(decision.source.clone())
                }
                _ => None,
            })
            .flatten()
    }

    /// Record a redirect the loop's own permission review wrote — no person or agent at a
    /// gate — as the call's gate decision, under [`review_source`] at the level in force,
    /// and return it as the approval it is (docs/permissions.md).
    pub(crate) fn review_redirect(&self, id: &CallId, message: String) -> Approval {
        let decision = Approval::Redirect(message);
        let source = review_source(self.resolved_review_level());
        let record =
            crate::record::log::GateDecision::new(id.clone(), decision.clone(), Some(source));
        let entry = Payload::Telemetry(crate::record::log::Telemetry::GateDecision(record));
        // The record must not stop what it describes.
        if let Err(e) = self.append(self.latest_turn(), entry) {
            log::warn!("gate decision record: append failed: {e}");
        }
        decision
    }

    /// Deliver a client's decision to the waiting loop. Clears the gate so
    /// `GET /state` reflects the resolution immediately. `false` → no such gate.
    /// The decision is a person's action (docs/control-api.md), noted once the gate is
    /// known to be this one and before the decision reaches the loop, so the cold check
    /// that follows finds it.
    ///
    /// `decider` says who decided, spelled as a turn's `source` is, and the turn carrying
    /// an allow's note. The decision is recorded as a `Telemetry::GateDecision` at the same
    /// moment, so it precedes what the loop does with it (docs/log-and-projection.md).
    pub(crate) fn resolve_approval(
        &self,
        id: &CallId,
        decision: Approval,
        decider: Decider,
    ) -> bool {
        let sender = self.approvals.lock().unwrap().remove(id);
        let Some(tx) = sender else { return false };
        // A loop that already moved on dropped its receiver: nothing was decided.
        if !tx.is_closed() {
            let mut record =
                crate::record::log::GateDecision::new(id.clone(), decision.clone(), decider.source);
            record.note_submission = decider.note_submission;
            let entry = Payload::Telemetry(crate::record::log::Telemetry::GateDecision(record));
            // The record must not stop what it describes.
            if let Err(e) = self.append(self.latest_turn(), entry) {
                log::warn!("gate decision record: append failed: {e}");
            }
        }
        self.person_acted(PersonAction::Decision {
            aborted: matches!(decision, Approval::Abort),
        });
        {
            let mut pending = self.pending.lock().unwrap();
            if pending.as_ref().is_some_and(|p| p.gate.call_id == id.0) {
                *pending = None;
            }
        }
        // The decision ends the wait now, so a `settle` racing the loop's wake does not
        // read a gate that is already answered (`phase.rs`).
        self.end_wait(|phase| phase == LoopPhase::Gated, LoopPhase::Executing);
        // The gate topic and the status watchers (`subscriptions.rs`) read the
        // resolution now rather than at the loop's next append, which on an
        // approved long-running command is some time away.
        self.touch_state();
        // The receiver is dropped only if the loop already moved on (e.g. the turn
        // was aborted by another route); treat that as "nothing waiting".
        tx.send(decision).is_ok()
    }
}
