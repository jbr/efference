//! The loop's [`Frontend`], backed entirely by [`Session`] — every input arrives
//! over HTTP (docs/control-channel.md).

use super::*;

use crate::control_tools::BoundAction;

/// The loop's [`Frontend`], backed entirely by [`Session`]: every input arrives
/// over HTTP. The loop is pull-shaped — it blocks at `read_user_turn` / `approve`
/// — and each pull resolves when a client pushes.
pub struct RemoteFrontend {
    pub(super) shared: Arc<Session>,
    /// Single-consumer receiver of the turn inbox.
    pub(super) turns: mpsc::UnboundedReceiver<SubmittedTurn>,
}

impl Frontend for RemoteFrontend {
    async fn read_user_turn(&mut self) -> Option<SubmittedTurn> {
        // Mark the loop parked so `/settle` reports idle while we wait (and wake any waiter).
        self.shared.going_idle();
        loop {
            // Blocks until a `POST /turns` arrives. `None` (all senders dropped) only
            // happens once the daemon is tearing down, which ends the loop cleanly.
            let turn = self.turns.recv().await?;
            // The channel cannot be searched, so a withdrawn turn still surfaces here; the
            // pending queue is the authority (docs/control-channel.md). An unclaimed turn was
            // recalled while in flight — drop it and keep waiting, still parked.
            if self.shared.claim_turn(&turn.submission_id, true) {
                return Some(turn);
            }
        }
    }

    fn drain_queued_turns(&mut self) -> Vec<SubmittedTurn> {
        // Non-blocking pull of everything queued right now (docs/control-channel.md).
        // Each drained turn is claimed off the pending queue — the loop is already running, so
        // `running` is left alone (unlike the idle path), or `/settle` would read "busy" forever
        // after the trajectory ends. A turn withdrawn while in flight fails the claim and is
        // dropped. Blank turns are dropped (still claimed): a mid-trajectory empty submit mirrors
        // the idle path's blank-keep-idling, never landing an empty `User` entry on the wire.
        let mut turns = Vec::new();
        while let Ok(turn) = self.turns.try_recv() {
            if self.shared.claim_turn(&turn.submission_id, false) && !turn.is_blank() {
                turns.push(turn);
            }
        }
        turns
    }

    async fn approve(&mut self, call: &ToolCall, bound: Option<&BoundAction>) -> Approval {
        let rx = self.shared.register_pending(call, bound);
        // A dropped sender (shouldn't happen on the live daemon) defaults to abort
        // — never run a call we couldn't get a decision for.
        rx.await.unwrap_or(Approval::Abort)
    }

    async fn interrupted(&mut self) -> Interrupt {
        // Race the two operator intents over the running tool: `POST /loop/interrupt`
        // kills, `PATCH /processes/:id {background: true}` detaches into the supervisor (docs/supervisor.md).
        tokio::select! {
            _ = self.shared.interrupt.notified() => Interrupt::Kill,
            _ = self.shared.detach.notified() => Interrupt::Detach,
        }
    }
}
