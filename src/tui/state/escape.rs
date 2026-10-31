//! The graduated escape ladder — return, pause, interrupt — and background detach (docs/tui.md).

use super::*;

impl<'a> InteractiveState<'a> {
    /// Graduated Esc (docs/tui.md): a **state machine**, not a press-count — each Esc's effect is a
    /// function of the current state, so there is no counter, timer, or reset. The rungs below run
    /// gentlest first, and the doc names what each one is for.
    pub(in crate::tui) fn handle_escape(&mut self) {
        // An active inline widget (the pace stepper) is the gentlest thing Esc can
        // undo — a sub-state revert, before any return/pause/interrupt rung.
        if self.pace_widget.is_some() {
            self.revert_pace_widget();
            return;
        }
        // An open session drawer is not at rest, however the composer flag stands:
        // the drawer closes before any pause or interrupt rung can fire.
        let at_rest_composer =
            self.composing && !self.sidebar_open && matches!(self.ui_mode, UiMode::Normal);
        if !at_rest_composer {
            self.normalize_to_composer();
            return;
        }
        let held = self.topics.pace.is_some_and(|p| p.held);
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
            // The mode replacement above is the whole step for both: the help screen
            // just closes, and the switcher closes with any confirm it was holding,
            // which is the direction Esc is allowed to move a destructive verb.
            UiMode::Help | UiMode::Sessions(_) => {}
            UiMode::ThemePicker {
                saved_theme_name, ..
            } => {
                let _ = self.apply_theme(&saved_theme_name);
            }
            UiMode::Normal => {
                self.viewport.keyboard_cursor = KeyboardCursor::VirtualTop;
                // Sidebar focus rides this rung home too: it is "not at rest in the
                // composer", and Esc is one-directional toward the composer.
                self.close_sidebar();
                self.sidebar_return = None;
                // A read-only view has no composer to return to (docs/tui.md), so its
                // steady state is the strip — the only surface still carrying verbs.
                if let Some(label) = &self.viewing {
                    self.composing = false;
                    self.strip_focus = self.leading_strip_verb();
                    self.ui.debug_message = view_status(label).into();
                } else {
                    self.composing = true;
                    self.strip_focus = None;
                    self.ui.debug_message = "compose — ⏎ send · ^J newline · ↑ navigate".into();
                }
            }
        }
    }

    /// The pause step of graduated Esc (docs/tui.md): set the `held` overlay (unit 2) and push it to
    /// the daemon so the loop stops at the next juncture. Idempotent (ensures held whatever the current
    /// pace), optimistic like the pace keys. The hint names both ways forward: another Esc stops a
    /// running tool now, and typing a message resumes (submit always resumes, unit 2).
    fn esc_hold(&mut self) {
        let cur = self.topics.pace.unwrap_or(PaceView {
            speed_ms: 0,
            held: false,
        });
        let _ = self.pace_tx.send((None, Some(true)));
        self.topics.pace = Some(PaceView { held: true, ..cur });
        self.ui.debug_message =
            "⏸ HELD — esc again to stop the running tool · type to resume".into();
        self.dirty = true;
    }

    /// The interrupt step of graduated Esc (docs/tui.md): fire `POST /loop/interrupt`, which stops the
    /// loop at the next juncture and withholds the in-flight call's output from the model
    /// (docs/control-channel.md) — plus, on a foreground bash, kills the running process group;
    /// with no call in flight it ends the generation itself. The hint names whichever promise
    /// applies, and all end the same way: the operator's next message is what the model reads.
    pub(in crate::tui) fn esc_interrupt(&mut self) {
        let _ = self.interrupt_tx.send(Interrupt::Kill);
        self.ui.debug_message = match self.in_flight_call() {
            Some(call) if crate::bash::is_foreground_call(&call) => {
                "interrupting — killing the running command · type to steer".into()
            }
            Some(_) => "interrupting — its output won't reach the model · type to steer".into(),
            None => "interrupting — ending this generation · type to steer".into(),
        };
        self.dirty = true;
    }

    /// The foreground→background handoff key (docs/process-monitor.md): ask the daemon
    /// to detach the in-flight tool into the background process monitor — keep it
    /// running as a `p#<n>` process instead of killing it or waiting it out.
    /// Fire-and-forget like the interrupt: the daemon no-ops if nothing is
    /// mid-tool. A message typed right after is delivered mid-task, so it reaches
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
    /// Optimistic like [`esc_hold`](Self::esc_hold).
    pub(in crate::tui) fn toggle_pace_hold(&mut self) {
        let cur = self.topics.pace.unwrap_or(PaceView {
            speed_ms: 0,
            held: false,
        });
        let held = !cur.held;
        let _ = self.pace_tx.send((None, Some(held)));
        self.topics.pace = Some(PaceView { held, ..cur });
        self.ui.debug_message = if held {
            "paused — ⏸ HELD (resume: same key, or send a message)".into()
        } else {
            "resumed".into()
        };
        self.dirty = true;
    }
}
