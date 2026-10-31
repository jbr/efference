//! The `verbs` state topic (docs/control-api.md): what each loop route would answer
//! now, read from the same function the route calls.
//!
//! One entry per route that can refuse, plus the turn, which never does. A client
//! renders a refused verb disabled with the route's own message, so the table and the
//! route cannot disagree; a request that arrives after the table moved is checked
//! again by the route.

use super::*;
use serde_json::json;

impl Session {
    /// Whether a turn submitted now is delivered at once — to the idle loop, as the
    /// answer to a pending `ask`, at a cold park or a held pace it releases, or at a pace
    /// delay it ends — or waits in the queue for the loop's next juncture. The phase and the
    /// queue come from one [`phase_and_queue`](Self::phase_and_queue), so a turn the loop is
    /// claiming reads as one the new turn waits behind.
    pub(crate) fn turn_delivery(&self) -> &'static str {
        let now = self.phase_and_queue();
        let at_once = match now.phase {
            LoopPhase::Idle => !now.queued,
            LoopPhase::Asking
            | LoopPhase::ColdParked
            | LoopPhase::Blocked {
                reason: BlockedReason::PaceHeld,
            } => true,
            // A pace delay ends on a submit; the pause between upstream attempts does not.
            LoopPhase::Delayed { .. } => !now.retry_pause,
            _ => false,
        };
        if at_once { "now" } else { "queued" }
    }

    /// The `verbs` topic: `turn`, `continue`, and `compact`, each `{accepted: true, …}` or
    /// `{accepted: false, message}` with the refusal the route would give.
    pub(super) fn verbs_json(&self) -> Value {
        let refused = |message: &str| json!({ "accepted": false, "message": message });
        let continue_verb = match self.continue_verdict() {
            Ok(_) => json!({ "accepted": true }),
            Err(why) => refused(why.message()),
        };
        // `warm` names the body the route accepts: `true` when the cache is believed
        // live, so only a request that says `warm: true` compacts.
        let compact_verb = match self.compaction_verdict(false) {
            Ok(_) => json!({ "accepted": true, "warm": false }),
            Err(super::compact::NotCompactable::NotCold) => match self.compaction_verdict(true) {
                Ok(_) => json!({ "accepted": true, "warm": true }),
                Err(why) => refused(why.message()),
            },
            Err(why) => refused(why.message()),
        };
        json!({
            "turn": { "accepted": true, "delivery": self.turn_delivery() },
            "continue": continue_verb,
            "compact": compact_verb,
        })
    }
}
