//! In-flight tool-call approvals awaiting a decision (docs/permissions.md).

use super::*;

/// An in-flight approval surfaced to clients via `GET /state` (the ephemeral
/// control state the log doesn't record — it stores only the *result*).
#[derive(Clone, Serialize)]
pub(super) struct Pending {
    pub(super) call_id: String,
    pub(super) name: String,
    pub(super) input: Value,
    /// The spotter's one-line reason for routing this call to the human, if it gated via the
    /// spotter (docs/permissions.md) — read from the `Spot` audit entry the spotter logs *before*
    /// the gate. Lets the client show *why* a call is held without a new side channel. `None` when
    /// no spotter verdict preceded the gate.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) reason: Option<String>,
    /// The leaf action this approval authorizes, when the held call is a bound
    /// `agents__decide` (docs/permissions.md). `None` for an ordinary gate.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) acting_on: Option<crate::control_tools::BoundAction>,
}

/// A session's held call, as its parent's `agents__decide` reads it
/// (docs/permissions.md): the call the model wrote, its transcript handle, and the
/// directories its action is classified against.
pub(crate) struct PendingGate {
    pub(crate) call: ToolCall,
    /// The handle a decision names it by (`seq` or `seq.n`).
    pub(crate) handle: Option<String>,
    pub(crate) directories: crate::policy::Directories,
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
    pub(super) fn latest_spot(&self, call_id: &CallId) -> Option<crate::log::Spot> {
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
                pending.name.clone(),
                pending.call_id.clone(),
                pending.input.clone(),
            )
        };
        Some(PendingGate {
            handle: self.handle_of(&call.id.0),
            call,
            directories: self.directories(),
        })
    }

    /// Register an approval gate and surface it to `GET /state`. Returns the
    /// receiver the loop awaits. `bound` is the leaf action an `agents__decide`
    /// authorizes, when the held call is one (docs/permissions.md).
    pub(super) fn register_pending(
        &self,
        call: &ToolCall,
        bound: Option<&crate::control_tools::BoundAction>,
    ) -> oneshot::Receiver<Approval> {
        let (tx, rx) = oneshot::channel();
        self.approvals.lock().unwrap().insert(call.id.clone(), tx);
        let reason = self.latest_spot_reason(&call.id);
        // Composed before the `Pending` takes ownership of the fields. A bound
        // approval names the child's leaf action rather than the approval call.
        let push_body = super::push::gate_body(&call.name, call.intent(), reason.as_deref(), bound);
        *self.pending.lock().unwrap() = Some(Pending {
            call_id: call.id.0.clone(),
            name: call.name.clone(),
            input: call.input.clone(),
            reason,
            acting_on: bound.cloned(),
        });
        // A gate is a settled state: the phase wakes any `/settle` waiter (the `?wait` on
        // `/turn` returns here). Moved after the gate is stored, so a waiter it wakes reads it.
        self.set_phase(LoopPhase::Gated);
        // …and one of the two moments worth a push (docs/web-push.md). Spawned, so
        // the loop reaches its `rx.await` without waiting on a push service.
        self.push(crate::log::MomentKind::Gate, push_body);
        rx
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
                Payload::Telemetry(crate::log::Telemetry::GateDecision(decision))
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
    ///
    /// [`review_source`]: crate::frontend::review_source
    pub(crate) fn review_redirect(&self, id: &CallId, message: String) -> Approval {
        let decision = Approval::Redirect(message);
        let source = crate::frontend::review_source(self.resolved_review_level());
        let record = crate::log::GateDecision::new(id.clone(), decision.clone(), Some(source));
        let entry = Payload::Telemetry(crate::log::Telemetry::GateDecision(record));
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
    /// `source` is the decider, spelled as a turn's `source` is (`None` for the
    /// operator). The decision is recorded as a `Telemetry::GateDecision` at the same
    /// moment, so it precedes what the loop does with it (docs/log-and-projection.md).
    /// `note_submission` is the submission id of the turn carrying an allow's note,
    /// which the decider submits after the decision lands (`decide`, docs/tools.md).
    pub(crate) fn resolve_approval(
        &self,
        id: &CallId,
        decision: Approval,
        source: Option<String>,
        note_submission: Option<String>,
    ) -> bool {
        let sender = self.approvals.lock().unwrap().remove(id);
        let Some(tx) = sender else { return false };
        // A loop that already moved on dropped its receiver: nothing was decided.
        if !tx.is_closed() {
            let mut record = crate::log::GateDecision::new(id.clone(), decision.clone(), source);
            record.note_submission = note_submission;
            let entry = Payload::Telemetry(crate::log::Telemetry::GateDecision(record));
            // The record must not stop what it describes.
            if let Err(e) = self.append(self.latest_turn(), entry) {
                log::warn!("gate decision record: append failed: {e}");
            }
        }
        self.person_acted(super::PersonAction::Decision {
            aborted: matches!(decision, Approval::Abort),
        });
        {
            let mut pending = self.pending.lock().unwrap();
            if pending.as_ref().is_some_and(|p| p.call_id == id.0) {
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
