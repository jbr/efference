//! Holding a triage gate until it has a verdict (docs/redaction-fork.md).
//!
//! A gate that produced no verdict is never shown to the agent. The results stay in
//! the loop, unappended, while the trajectory is stalled on the `stall` topic; the
//! operator's continue ("retry now") runs the gate again from the top, and the
//! operator's interrupt ends the turn with the results withheld the way the
//! interrupt's *after* juncture withholds one. The loop is still running throughout,
//! so a host that stops here loses the results as it would mid-tool.

use super::*;

use crate::distill::{GateFailure, GateOutcome, PEEK_FORKS, PeekFailure};

/// What the held gate's attempts accumulated before one of them succeeded: carried
/// onto the successful gate's `Triage`, so a recovered hold is still in the record.
#[derive(Default)]
pub(super) struct Carried {
    retries: Vec<String>,
    thinking: Vec<String>,
}

impl Carried {
    /// The successful gate's outcome, with the held attempts' breadcrumbs and
    /// reasoning ahead of its own.
    pub(super) fn onto(mut self, mut outcome: GateOutcome) -> GateOutcome {
        self.retries.append(&mut outcome.retries);
        self.thinking.append(&mut outcome.thinking);
        outcome.retries = self.retries;
        outcome.thinking = self.thinking;
        outcome
    }

    /// The outcome of a gate the operator interrupted (docs/control-channel.md).
    pub(super) fn interrupted(self, count: usize) -> GateOutcome {
        GateOutcome::interrupted(count, self.retries, self.thinking)
    }
}

/// How a held gate resumed.
pub(super) enum Held {
    /// Run the gate again: the operator asked to retry now.
    Retry,
    /// The operator interrupted; the latch stays set for the juncture after the gate,
    /// which ends the turn.
    Interrupted,
}

/// Hold a gate that produced no verdict until the operator retries or interrupts.
/// Records the failed attempts' usage, carries their breadcrumbs, and publishes the
/// hold as a stall while it lasts.
pub(super) async fn hold(
    shared: &Arc<Session>,
    client: &Client,
    failure: GateFailure,
    carried: &mut Carried,
) -> Held {
    let GateFailure {
        failure,
        usage,
        retries,
        thinking,
    } = failure;
    for u in usage {
        shared.record_request(RequestOrigin::Peek, u);
    }
    carried.retries.extend(retries);
    carried.thinking.extend(thinking);
    let (reason, message, why) = match failure {
        PeekFailure::Interrupted => return Held::Interrupted,
        PeekFailure::Upstream(why) => (
            StallReason::TriageUnavailable,
            "triage could not reach its model — the results are held until it does",
            why,
        ),
        PeekFailure::Unusable(why) => {
            log::error!(
                "harness defect: triage produced no usable verdict in {PEEK_FORKS} forks; \
                 holding the results — {why}"
            );
            (
                StallReason::TriageDefect,
                "triage produced no usable verdict (a harness defect) — the results are held",
                why,
            )
        }
    };
    eprintln!("[{message}: {why}]");
    carried.retries.push(format!("held: {why}"));
    shared.set_stall(stalled(reason, None, message.to_string(), why));

    // Armed before the latch is read, so an interrupt between the two still wakes this.
    let interrupt = shared.interrupt_signal();
    let interrupted = interrupt.notified();
    tokio::pin!(interrupted);
    interrupted.as_mut().enable();
    let retry = client.upstream_retry();
    let held = if shared.interrupt_pending() {
        Held::Interrupted
    } else {
        tokio::select! {
            _ = retry.pause(None) => Held::Retry,
            () = &mut interrupted => Held::Interrupted,
        }
    };
    shared.clear_stall();
    held
}
