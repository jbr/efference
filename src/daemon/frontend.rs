//! The loop's [`Frontend`], backed entirely by [`Shared`] — every input arrives
//! over HTTP (docs/control-channel.md).

use super::*;

/// The loop's [`Frontend`], backed entirely by [`Shared`]: every input arrives
/// over HTTP. The loop body is unchanged — it still pulls (blocks at
/// `read_user_turn` / `approve`), and the pull resolves when a client pushes.
pub struct RemoteFrontend {
    pub(super) shared: Arc<Shared>,
    /// Single-consumer receiver of the turn inbox.
    pub(super) turns: mpsc::UnboundedReceiver<String>,
}

impl Frontend for RemoteFrontend {
    async fn read_user_turn(&mut self) -> Option<String> {
        // Mark the loop parked so `/settle` reports idle while we wait (and wake any waiter).
        self.shared.parking();
        // Blocks until a `POST /turns` arrives. `None` (all senders dropped) only
        // happens once the daemon is tearing down, which ends the loop cleanly.
        let turn = self.turns.recv().await;
        if turn.is_some() {
            // Back to busy: mark running and consume the queued count.
            self.shared.dequeued_turn();
        }
        turn
    }

    fn drain_injected_turns(&mut self) -> Vec<String> {
        // Non-blocking pull of everything queued right now (docs/control-channel.md).
        // Each drained turn consumes its `queued` count — the loop is already running, so we
        // only balance the counter (never touch `running`), or `/settle` would read "busy"
        // forever after the trajectory ends. Blank turns are dropped (still counted): a
        // mid-trajectory empty submit mirrors the idle path's blank-keep-idling, never landing
        // an empty `User` entry on the wire.
        let mut turns = Vec::new();
        while let Ok(text) = self.turns.try_recv() {
            self.shared.injected_turn_dequeued();
            if !text.trim().is_empty() {
                turns.push(text);
            }
        }
        turns
    }

    async fn approve(&mut self, call: &ToolCall) -> Approval {
        let rx = self.shared.register_pending(call);
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
