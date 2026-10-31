//! Why a session stopped making progress, and the config repair that holds its
//! loop (docs/control-api.md).

use super::*;

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
