//! Folding a `state` frame into the topic store, and what the TUI derives from the store
//! and the clock (docs/tui.md).
//!
//! A frame replaces its topic's slot whole (`topics.rs`). Most topics are chrome the
//! renderers read from the slot, and their arrival only asks for a repaint. The few
//! whose arrival also does something have an arm in [`apply_topic`](InteractiveState::apply_topic):
//! the gate and the ask change how a committed call renders, the processes carry their
//! collected output across the replacement, the identity can name a successor to follow,
//! and a failed start hands turns back to the composer.
//!
//! What depends on the clock — the process rows' elapsed times, the time since the last
//! request, the operations text — is derived here, after every frame and once a second,
//! from the absolute instants the topics carry (docs/control-api.md). The wire carries no
//! ticking values, and deriving them here keeps rendering a pure function of state.

use super::*;
use crate::protocol::state::ProcessState;
use crate::protocol::state::{Gate, LoopPhase, SessionIdentity};
use crate::tui::process_panel;
use crate::tui::topics::TopicUpdate;

impl InteractiveState<'_> {
    /// Fold one `state` frame: replace the topic's slot with the value it carried, do
    /// what that topic's arrival means, and ask for a repaint. Returns whether the UI loop
    /// should end, which only a followed hand-off asks for.
    pub(super) fn apply_topic(&mut self, update: TopicUpdate) -> bool {
        let mut leave = false;
        match update {
            TopicUpdate::Gate(gate) => self.apply_gate(gate),
            TopicUpdate::Ask(ask) => self.apply_ask(ask),
            TopicUpdate::Processes(processes) => self.apply_processes(processes),
            TopicUpdate::SessionIdentity(identity) => {
                leave = self.apply_session_identity(identity);
            }
            TopicUpdate::Starting(starting) => self.apply_starting(starting),
            // Everything else is chrome the renderers read from the slot.
            update => self.topics.replace(update),
        }
        self.refresh_derived();
        self.dirty = true;
        leave
    }

    /// The gate renders inline in the transcript (the tail call shows `⏸ awaiting
    /// approval`, docs/tui.md), so a gate change must reproject, unlike the other
    /// ephemeral chrome. The status bar still mirrors the affordance for the
    /// scrolled-away case, and drops the hint when the gate resolves.
    fn apply_gate(&mut self, gate: Option<Gate>) {
        self.ui.debug_message = match &gate {
            Some(held) => format!(
                "approval needed: {} — ⏎ approve · type to redirect",
                held.name
            )
            .into(),
            None => IDLE_STATUS.into(),
        };
        self.topics.gate = gate;
        self.mark_reproject();
    }

    /// The panel's rows are rebuilt from the replacement, each keeping the tail this
    /// client collected for its process (`process_panel.rs`). The running tail is never in
    /// the log, so this repaints without a reproject (docs/process-monitor.md).
    fn apply_processes(&mut self, processes: Vec<ProcessState>) {
        self.process_panel = process_panel::panel(&processes, &self.process_panel);
        self.topics.processes = processes;
    }

    /// A session that handed off (docs/tools.md) is followed to its successor: the same
    /// exit the session switcher takes, so the attach loop re-enters on the successor's
    /// prefix. Returns whether to leave.
    fn apply_session_identity(&mut self, identity: Option<SessionIdentity>) -> bool {
        let successor = identity.as_ref().and_then(|i| i.successor.clone());
        let follow = self.switch_to.is_none() && successor.is_some();
        if follow {
            self.switch_to = Some(Exit::Switch {
                id: successor.unwrap_or_default(),
            });
        }
        self.topics.session_identity = identity;
        follow
    }

    /// Re-derive what the topics and the clock determine together.
    fn refresh_derived(&mut self) {
        self.refresh_operation_text();
        self.refresh_since_last_request();
    }

    /// Recompute every clock-derived display value from the absolute stamps it was
    /// derived from: each process row's running/idle seconds, the time since the last
    /// request, and the operations text. Returns whether anything moved.
    ///
    /// Called once a second by the UI loop's clock tick. The wire stopped carrying
    /// ticking values so the level-triggered `state` topics could stay quiet
    /// (docs/control-api.md); the elapsed *display* still has to advance, and this is
    /// where it does.
    pub(in crate::tui) fn refresh_clocks(&mut self) -> bool {
        let mut changed = false;
        for p in &mut self.process_panel {
            let before = (p.running_secs, p.idle_secs);
            p.refresh_elapsed();
            changed |= (p.running_secs, p.idle_secs) != before;
        }
        changed |= self.refresh_operation_text();
        changed |= self.refresh_since_last_request();
        if changed {
            self.dirty = true;
        }
        changed
    }

    /// Seconds since the last upstream request, from `usage.last_request_at`. Returns
    /// whether the value changed.
    fn refresh_since_last_request(&mut self) -> bool {
        let secs = self
            .topics
            .usage
            .as_ref()
            .and_then(|usage| usage.last_request_at)
            .map(|at| crate::tui::secs_since(at.0));
        let changed = secs != self.since_last_request;
        self.since_last_request = secs;
        changed
    }

    /// The status bar's operation slot: while the loop is running, a pending gate or `ask`
    /// reads as the operator being waited on, ahead of whatever request or tool is open (docs/tui.md).
    pub(crate) fn status_operation_text(&self) -> Option<String> {
        let pending = crate::tui::operations::pending_text(
            self.topics.loop_phase == LoopPhase::Gated,
            self.topics.loop_phase == LoopPhase::Asking,
        );
        pending
            .map(str::to_string)
            .or_else(|| self.operation_text.clone())
    }

    /// Re-derive the status bar's operation text from the topic, the session's model,
    /// and the clock. Shown only while the loop is running: an idle triage or a
    /// keepalive ping is not something the operator is waiting on. Returns whether
    /// the text changed.
    pub(super) fn refresh_operation_text(&mut self) -> bool {
        let text = match (&self.topics.current_operations, self.loop_running()) {
            (Some(operations), true) => {
                let model = self
                    .topics
                    .session_identity
                    .as_ref()
                    .map_or("the model", |identity| identity.model.as_str());
                crate::tui::operations::operation_text(
                    operations,
                    model,
                    crate::record::log::Timestamp::now().0,
                )
            }
            _ => None,
        };
        let changed = text != self.operation_text;
        self.operation_text = text;
        if changed {
            self.dirty = true;
        }
        changed
    }
}

#[cfg(test)]
mod test;
