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
}

impl Shared {
    /// The spotter's reason for the most recent verdict on `call_id`, read from the log's `Spot`
    /// audit entries (docs/permissions.md). The spotter logs its verdict *before* routing the call
    /// to the human gate, so by the time the gate registers, the reason is already durably in the
    /// log — no side channel needed. The log is the source of truth, here too.
    pub(super) fn latest_spot_reason(&self, call_id: &CallId) -> Option<String> {
        self.log
            .lock()
            .unwrap()
            .entries()
            .iter()
            .rev()
            .find_map(|e| match &e.payload {
                Payload::Spot(s) if &s.call_id == call_id => Some(s.reason.clone()),
                _ => None,
            })
    }

    /// Register an approval gate and surface it to `GET /state`. Returns the
    /// receiver the loop awaits.
    pub(super) fn register_pending(&self, call: &ToolCall) -> oneshot::Receiver<Approval> {
        let (tx, rx) = oneshot::channel();
        self.approvals.lock().unwrap().insert(call.id.clone(), tx);
        let reason = self.latest_spot_reason(&call.id);
        // Composed before the `Pending` takes ownership of the two fields.
        let push_body = super::push::gate_body(&call.name, call.intent(), reason.as_deref());
        *self.pending.lock().unwrap() = Some(Pending {
            call_id: call.id.0.clone(),
            name: call.name.clone(),
            input: call.input.clone(),
            reason,
        });
        // A gate is a settled state — wake any `/settle` waiter (the `?wait` on `/turn` returns here).
        self.settle_changed.notify_waiters();
        // …and one of the two moments worth a push (docs/web-push.md). Spawned, so
        // the loop reaches its `rx.await` without waiting on a push service.
        self.push("Gate", push_body);
        rx
    }

    /// Deliver a client's decision to the waiting loop. Clears the gate so
    /// `GET /state` reflects the resolution immediately. `false` → no such gate.
    pub(super) fn resolve_approval(&self, id: &CallId, decision: Approval) -> bool {
        let sender = self.approvals.lock().unwrap().remove(id);
        let Some(tx) = sender else { return false };
        {
            let mut pending = self.pending.lock().unwrap();
            if pending.as_ref().is_some_and(|p| p.call_id == id.0) {
                *pending = None;
            }
        }
        // The receiver is dropped only if the loop already moved on (e.g. the turn
        // was aborted by another route); treat that as "nothing waiting".
        tx.send(decision).is_ok()
    }
}
