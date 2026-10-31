//! Holding a triage until it has a verdict (docs/triage.md).
//!
//! A triage that produced no verdict is never shown to the agent. The results stay in
//! the loop, unappended, while the trajectory is stalled on the `stall` topic; the
//! operator's continue ("retry now") runs the triage again from the top, and the
//! operator's interrupt ends the turn with the results withheld the way the
//! interrupt's *after* juncture withholds one. The loop is still running throughout,
//! so a host that stops here loses the results as it would mid-tool.

use super::*;

use crate::triage::{HoldFailure, HoldOutcome, TRIAGE_RESTARTS, TriageFailure};

/// What the held triage's attempts accumulated before one of them succeeded: carried
/// onto the successful triage's `Triage`, so a recovered hold is still in the record.
#[derive(Default)]
pub(super) struct Carried {
    retries: Vec<String>,
    thinking: Vec<String>,
}

impl Carried {
    /// The successful triage's outcome, with the held attempts' breadcrumbs and
    /// reasoning ahead of its own.
    pub(super) fn onto(mut self, mut outcome: HoldOutcome) -> HoldOutcome {
        self.retries.append(&mut outcome.retries);
        self.thinking.append(&mut outcome.thinking);
        outcome.retries = self.retries;
        outcome.thinking = self.thinking;
        outcome
    }

    /// The outcome of a triage an interrupt stopped (docs/control-channel.md), `by` who
    /// sent it.
    pub(super) fn interrupted(self, count: usize, by: Option<&str>) -> HoldOutcome {
        HoldOutcome::interrupted(count, by, self.retries, self.thinking)
    }
}

/// How a held triage resumed.
pub(super) enum Held {
    /// Run the triage again: the operator asked to retry now.
    Retry,
    /// The operator interrupted; the latch stays set for the juncture after the triage,
    /// which ends the turn.
    Interrupted,
}

/// Hold a triage that produced no verdict until the operator retries or interrupts.
/// Records the failed attempts' usage, carries their breadcrumbs, and publishes the
/// hold as a stall while it lasts. `on_agent_client` is whether the failed triages ran on
/// the agent's own client, as the triage's successful ones did.
pub(super) async fn hold(
    shared: &Arc<Session>,
    client: &Client,
    on_agent_client: bool,
    failure: HoldFailure,
    carried: &mut Carried,
) -> Held {
    let HoldFailure {
        failure,
        usage,
        retries,
        thinking,
        sent,
    } = failure;
    for u in usage {
        shared.record_triage(on_agent_client, u);
    }
    // A request that got a response wrote the triage lineage's prefix whatever its verdict
    // (docs/transport.md).
    if let Some(sent) = sent {
        shared.store_triage_request(sent.request, sent.at, &sent.usage);
    }
    carried.retries.extend(retries);
    carried.thinking.extend(thinking);
    let (reason, message, why) = match failure {
        TriageFailure::Interrupted => return Held::Interrupted,
        TriageFailure::Upstream(why) => (
            StallReason::TriageUnavailable,
            "triage could not reach its model — the results are held until it does",
            why,
        ),
        TriageFailure::Unusable(why) => {
            log::error!(
                "harness defect: triage produced no usable verdict in {TRIAGE_RESTARTS} forks; \
                 holding the results — {why}"
            );
            (
                StallReason::TriageDefect,
                "triage produced no usable verdict (a harness defect) — the results are held",
                why,
            )
        }
    };
    log::debug!("{message}: {why}");
    carried.retries.push(format!("held: {why}"));
    shared.set_stall(stalled(reason, None, message.to_string(), why));
    // Blocked until the operator retries or interrupts, named after the stall it is
    // (docs/control-api.md); the continue that retries ends the wait (`request_continue`).
    let blocked = match reason {
        StallReason::TriageDefect => BlockedReason::TriageDefect,
        _ => BlockedReason::TriageUnavailable,
    };
    let _blocked = shared.wait_phase(
        LoopPhase::Blocked { reason: blocked },
        LoopPhase::Requesting,
    );

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
