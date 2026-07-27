//! The graduated escape ladder — return, pause, interrupt — and background detach (docs/tui.md).

use super::*;

impl<'a> InteractiveState<'a> {
    /// Graduated Esc (docs/tui.md): a **state machine**, not a press-count — each Esc's effect is a
    /// function of the current state, so no counter, timer, or reset is needed. Precedence, gentlest
    /// first:
    ///
    /// 1. **not at rest in the composer** (an overlay is open, or focus is in the transcript) →
    ///    return to the composer.
    /// 2. **at the composer, held** → interrupt the in-flight tool (`POST /interrupt`).
    /// 3. **at the composer, not held, loop active** → pause/hold.
    /// 4. **at the composer, not held, nothing happening** → swallow (Esc has nothing to do).
    ///
    /// So sitting in the composer while the agent works, one Esc pauses and a second interrupts;
    /// and jamming Esc from anywhere still walks return-to-composer → pause → interrupt.
    pub(in crate::tui) fn handle_escape(&mut self) {
        // An active inline widget (the pace stepper) and a stager are the gentlest
        // things Esc can undo — a sub-state revert, before any return/pause/interrupt
        // rung. The two are mutually exclusive (widget is strip-focused, stager
        // composer-focused).
        if self.pace_widget.is_some() {
            self.revert_pace_widget();
            return;
        }
        if self.staging.is_some() {
            self.cancel_staging();
            return;
        }
        let at_rest_composer = self.composing && matches!(self.ui_mode, UiMode::Normal);
        if !at_rest_composer {
            self.normalize_to_composer();
            return;
        }
        let held = self.pace.is_some_and(|p| p.held);
        if held {
            self.esc_interrupt();
        } else if self.is_loop_active() {
            self.esc_hold();
        }
        // else: nothing happening → swallow (a silent no-op, by design).
    }

    /// Return to the composer's steady state, from anywhere (docs/tui.md) — close an overlay (help /
    /// theme picker), or from transcript navigation refocus the composer. Esc is one-directional
    /// toward the composer; this is the gentlest Esc step and the first thing it does when the UI
    /// isn't already at rest there.
    fn normalize_to_composer(&mut self) {
        match std::mem::replace(&mut self.ui_mode, UiMode::Normal) {
            UiMode::Help => {}
            UiMode::ThemePicker {
                saved_theme_name, ..
            } => {
                let _ = self.apply_theme(&saved_theme_name);
            }
            UiMode::Normal => {
                self.composing = true;
                self.strip_focus = None;
                self.viewport.keyboard_cursor = KeyboardCursor::VirtualTop;
                self.ui.debug_message = "compose — ⏎ send · ^J newline · ↑ navigate".into();
            }
        }
    }

    /// The pause step of graduated Esc (docs/tui.md): set the `held` overlay (unit 2) and push it to
    /// the daemon so the loop stops at the next juncture. Idempotent (ensures held whatever the current
    /// pace), optimistic like the pace keys. The hint names both ways forward: another Esc stops a
    /// running tool now, and typing a message resumes (submit always resumes, unit 2).
    fn esc_hold(&mut self) {
        let cur = self.pace.unwrap_or(PaceView {
            speed_ms: 0,
            held: false,
        });
        let _ = self.pace_tx.send((None, Some(true)));
        self.pace = Some(PaceView { held: true, ..cur });
        self.ui.debug_message =
            "⏸ HELD — esc again to stop the running tool · type to resume".into();
        self.dirty = true;
    }

    /// The interrupt step of graduated Esc (docs/tui.md): fire `POST /interrupt`, which stops the
    /// loop at the next juncture and withholds the in-flight call's output from the model
    /// (docs/control-channel.md) — plus, on a foreground bash, kills the running process group.
    /// Reaches the one point pace can't (mid-tool). The hint names whichever promise applies, and
    /// both end the same way: the operator's next message is what the model reads.
    pub(in crate::tui) fn esc_interrupt(&mut self) {
        let _ = self.interrupt_tx.send(Interrupt::Kill);
        self.ui.debug_message = if self
            .in_flight_call()
            .is_some_and(crate::bash::is_foreground_call)
        {
            "interrupting — killing the running command · type to steer".into()
        } else {
            "interrupting — its output won't reach the model · type to steer".into()
        };
        self.dirty = true;
    }

    /// The foreground→background handoff key (docs/supervisor.md): ask the daemon
    /// to detach the in-flight tool into the background supervisor — keep it
    /// running as a `bg#<n>` process instead of killing it or waiting it out.
    /// Fire-and-forget like the interrupt: the daemon no-ops if nothing is
    /// mid-tool. A message typed right after rides the seam-drain, so it reaches
    /// the model alongside the handoff — no separate note-capture flow.
    pub(in crate::tui) fn background_detach(&mut self) {
        if self.is_loop_active() {
            let _ = self.interrupt_tx.send(Interrupt::Detach);
            self.ui.debug_message =
                "moving the running tool to the background… · type to follow up".into();
        } else {
            self.ui.debug_message = "nothing running to background".into();
        }
        self.dirty = true;
    }

    /// Toggle the pause overlay and push it to the daemon (docs/tui.md —
    /// the hold/resume key). The speed dial is retained underneath (resume returns to it).
    /// Optimistic like [`nudge_pace_speed`](Self::nudge_pace_speed).
    pub(in crate::tui) fn toggle_pace_hold(&mut self) {
        let cur = self.pace.unwrap_or(PaceView {
            speed_ms: 0,
            held: false,
        });
        let held = !cur.held;
        let _ = self.pace_tx.send((None, Some(held)));
        self.pace = Some(PaceView { held, ..cur });
        self.ui.debug_message = if held {
            "paused — ⏸ HELD (resume: same key, or send a message)".into()
        } else {
            "resumed".into()
        };
        self.dirty = true;
    }
}
