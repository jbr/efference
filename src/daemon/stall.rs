//! Why a session stopped making progress, and the config repair that holds its
//! loop (docs/control-api.md).

use super::*;

/// Why a trajectory stopped short of finishing (docs/transport.md).
#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum StallReason {
    /// The stream-abort budget was spent: every attempt died before finalizing.
    RetriesExhausted,
    /// The request itself failed after the transport's own retries.
    RequestFailed,
    /// The upstream named a failure no retry can change — an exhausted plan, a
    /// rejected request, a bad credential — so the loop stalled on the first
    /// attempt rather than spend the schedule (docs/transport.md).
    UpstreamRefused,
    /// The triage could not reach its model — the schedule ran out, or the
    /// upstream refused — so the loop holds the batch's results, unshown, until a
    /// retry succeeds or the operator interrupts (docs/triage.md).
    TriageUnavailable,
    /// The triage model answered every fork and never usably: a harness defect,
    /// logged as one. The loop holds the results the same way
    /// (docs/triage.md).
    TriageDefect,
}

/// A trajectory that ended without finishing (docs/transport.md).
///
/// The loop returns to idle on both failure paths above, which leaves the
/// daemon's other signals — the idle phase, no gate, an empty inbox once a
/// triage has dismissed the notification — identical to a run that
/// finished its work. This topic is the difference a supervising client reads:
/// set when the loop gives up, cleared when a trajectory next starts, so it
/// names the *current* condition rather than a past one (the same clear-on-
/// recovery rule the transport diagnostics follow).
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct Stall {
    pub reason: StallReason,
    /// How many attempts were spent, where the failure path counts them;
    /// `null` where it does not.
    pub attempts: Option<u32>,
    /// Unix millis at which the loop gave up — absolute, like every other
    /// instant on the control channel, so nothing has to re-emit a ticking
    /// field (docs/control-api.md).
    pub at: u64,
    /// Leads with what happened and what recovers it — the same register the
    /// retry topic's `message` uses.
    pub message: String,
    /// The upstream's own words for the failure that ended the trajectory.
    pub detail: String,
}

/// The config file on disk does not parse **and** could not be restored to the
/// last content that did (`config::Refreshed::RevertFailed`) — the one state the
/// adopted-or-rejected invariant cannot reach (docs/permissions.md).
///
/// It is terminal-until-repaired rather than a notice: the effective policy in
/// memory is still the last good config, but the file no longer shows it, and the
/// file is the perimeter's readable form. Running on indefinitely would mean a
/// session acting under permissions nobody can inspect. Making it terminal
/// introduces no blocked-with-nobody-home state, because a config write is the one
/// operation that is never autonomous — it gates for the orchestrator even under
/// `--autonomous`, so a file in this condition always has an owner.
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct ConfigRepair {
    /// The file a human has to fix.
    pub path: String,
    /// Unix millis at which the daemon parked, absolute like every other instant
    /// on the control channel (docs/control-api.md).
    pub at: u64,
    /// Leads with the condition and what clears it — the same register the
    /// `stall` topic's `message` uses.
    pub message: String,
    /// Why the file did not parse.
    pub parse_error: String,
    /// Why the restore of the last good content also failed.
    pub restore_error: String,
}

impl Session {
    /// Record that the trajectory gave up (docs/transport.md) — the loop is
    /// returning to idle without finishing. Read back by the `stall` topic.
    pub(crate) fn set_stall(&self, stall: Stall) {
        *self.stall.lock().unwrap() = Some(stall);
        // A stall is a decision point (`is_settled`): a held triage reaches it with the
        // loop still running, so the settle waiters are woken here, not only at park.
        self.settle_changed.notify_waiters();
        self.touch_state();
    }

    /// Clear the stall: a trajectory is starting, or a held triage is retrying, so the
    /// last outcome is no longer the current condition. Called when the work
    /// resumes rather than on a successful completion; the one time the topic is set
    /// while the loop runs is a held triage (docs/triage.md).
    pub(crate) fn clear_stall(&self) {
        let mut stall = self.stall.lock().unwrap();
        if stall.is_none() {
            return;
        }
        *stall = None;
        drop(stall);
        self.touch_state();
    }

    /// The trajectory-stalled condition, or `None` when the last trajectory
    /// finished (or none has run).
    pub(crate) fn stall(&self) -> Option<Stall> {
        self.stall.lock().unwrap().clone()
    }

    /// Park the loop on "config unparseable — human must repair"
    /// (docs/permissions.md). Idempotent: repeated failed saves replace the
    /// condition rather than stacking, exactly as the revert notice supersedes on
    /// its stable slot.
    pub(crate) fn require_config_repair(&self, repair: ConfigRepair) {
        *self.config_repair.lock().unwrap() = Some(repair);
        self.touch_state();
    }

    /// The file parses again: clear the park and wake whoever is waiting on it.
    /// Called from the same refresh that sets it, on every outcome that leaves
    /// valid content on disk — an adopted edit, or a revert that succeeded.
    pub(crate) fn clear_config_repair(&self) {
        let mut repair = self.config_repair.lock().unwrap();
        if repair.is_none() {
            return;
        }
        *repair = None;
        drop(repair);
        self.end_wait(
            |phase| {
                phase
                    == LoopPhase::Blocked {
                        reason: BlockedReason::ConfigRepair,
                    }
            },
            LoopPhase::Requesting,
        );
        self.config_repaired.notify_waiters();
        self.touch_state();
    }

    /// The config-repair condition, or `None` when the file parses.
    pub(crate) fn config_repair(&self) -> Option<ConfigRepair> {
        self.config_repair.lock().unwrap().clone()
    }

    /// Wait until the config file parses again (docs/permissions.md). Returns
    /// immediately when there is nothing to repair, which is the normal case.
    ///
    /// Lost-wakeup safe by the same discipline as the pace hold: the wake future is
    /// armed before the state is read, so a repair landing in between still wakes
    /// the wait. Re-checks after each wake, so a spurious notify cannot release a
    /// park that is still warranted.
    pub async fn await_config_repair(&self) {
        let mut blocked = None;
        loop {
            let repaired = self.config_repaired.notified();
            tokio::pin!(repaired);
            repaired.as_mut().enable(); // arm before reading, so a racing repair isn't lost
            let Some(repair) = self.config_repair() else {
                return;
            };
            log::debug!("config: parked — {}", repair.message);
            blocked.get_or_insert_with(|| {
                self.wait_phase(
                    LoopPhase::Blocked {
                        reason: BlockedReason::ConfigRepair,
                    },
                    LoopPhase::Requesting,
                )
            });
            repaired.await;
        }
    }
}
