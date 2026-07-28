//! The control strip (docs/tui.md): a one-row, always-present surface between the
//! transcript and the composer, showing the currently relevant loop verbs as
//! focusable, clickable, button-like items. It is the discoverable slow path and
//! the keybinding legend in one place — each item carries the key that would fire
//! the same verb without visiting the strip.
//!
//! Contents are **one relevance-ordered list per state**, most-relevant leftmost,
//! filled to the available width and **cut from the right** — so `↑ ⏎` from the
//! composer is always "do the most relevant thing," and what a narrow terminal
//! loses is always the least relevant verb. The list is the state's context verbs
//! followed by the omnipresent tail (`pace · theme · help · detach`), which is
//! therefore what gets cut first; every tail verb has a keyboard fallback, and
//! `pace` sits leftmost in the tail because it is a control, not a setting.
//!
//! Only verbs that apply in the state are shown — the strip flashing between
//! states is preferred to carrying an affordance that would do nothing. The strip
//! is chrome, not transcript: its contents come from the polled `/state` fields
//! the status bar reads plus the committed log, and it never projects.
//!
//! Focus model: the strip is one stop on the single vertical axis
//! (transcript → strip → composer). The focused item fills a reserved marker
//! cell with `▸` (a space when unfocused), so focus movement never shifts text
//! and stays visible in the colorless snapshot grid; the surface-color change is
//! asserted via `cell()`, not the grid.

use ratatui::{buffer::Buffer, layout::Rect};

use super::{InteractiveState, UiMode};
use crate::bash;

/// A verb the strip can carry. Dispatch happens in
/// [`activate_strip_item`](InteractiveState::activate_strip_item), always onto
/// mechanisms that also exist without the strip (pace, interrupt, gate approval).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum StripVerb {
    /// Toggle the pace hold (pause/resume).
    PauseToggle,
    /// Enter the inline pace stepper (the widget class, docs/tui.md).
    PaceWidget,
    /// Interrupt the loop (`POST /interrupt`) — shown as `kill` where it also
    /// kills a process group, `interrupt` everywhere else (docs/tui.md).
    Interrupt,
    /// Foreground→background handoff (`POST /background`).
    Background,
    /// Approve the pending gate.
    ApproveGate,
    /// Stage a `.allow` grant spec into the composer for editing (the stager
    /// class, docs/tui.md).
    AllowStage,
    /// Abort the pending gate.
    AbortGate,
    /// Open the theme picker.
    Theme,
    /// Open the help screen.
    Help,
    /// Leave the TUI; the daemon and session keep running.
    Detach,
}

/// One strip item: a label and the fast-path key it teaches (empty when there is
/// none). A bare-letter hint is also the item's **scoped mnemonic** — it fires the
/// verb while the strip is focused, and only where the item is shown (docs/tui.md).
#[derive(Debug, Clone, Copy)]
pub(super) struct StripItem {
    pub label: &'static str,
    pub hint: &'static str,
    pub verb: StripVerb,
}

const fn item(label: &'static str, hint: &'static str, verb: StripVerb) -> StripItem {
    StripItem { label, hint, verb }
}

/// The omnipresent tail, appended to every state's context verbs and therefore
/// the first thing a narrow width cuts (right to left: `detach`, `help`, `theme`,
/// `pace`). Ordered least-cuttable first: `pace` is a control the operator reaches
/// for mid-run, `detach` is the one with the plainest keyboard fallback.
const TAIL: [StripItem; 4] = [
    item("pace", "", StripVerb::PaceWidget),
    item("theme", "t", StripVerb::Theme),
    item("help", "?", StripVerb::Help),
    item("detach", "q", StripVerb::Detach),
];

/// The whole strip in a read-only view (docs/tui.md — `efference view`): the tail
/// verbs that act on the client alone, with `detach` reading `close` because there
/// is no daemon to leave running. `pace` is dropped with the rest of the loop verbs
/// — it is a control over a running loop, and there isn't one.
const VIEW_TAIL: [StripItem; 3] = [
    item("theme", "t", StripVerb::Theme),
    item("help", "?", StripVerb::Help),
    item("close", "q", StripVerb::Detach),
];

impl InteractiveState<'_> {
    /// The ranked item list for the current state. Re-evaluated per frame;
    /// contents only change when the underlying state does, so re-ranking happens
    /// at state transitions and never while the strip sits stable.
    ///
    /// Two transient sub-states own the row outright and so carry no tail — their
    /// `⏎`/`⎋` belong to the sub-state, and a clickable tail beside them would
    /// invite acting out from under it.
    pub(super) fn strip_items(&self) -> Vec<StripItem> {
        if self.read_only() {
            // No daemon behind the transcript: every loop verb would be an
            // affordance that does nothing, and the two sub-states below are
            // unreachable (neither a gate nor a pace dial exists here).
            return VIEW_TAIL.to_vec();
        }
        if self.pace_widget.is_some() {
            // The active pace stepper (docs/tui.md): ↑/↓ adjust, ⏎ sets, ⎋ reverts.
            return vec![item("pace", "", StripVerb::PaceWidget)];
        }
        if self.staging.is_some() {
            // Mid-staging the composer owns the interaction (⏎ commits, ⎋ cancels);
            // the row carries the staging note instead of items.
            return Vec::new();
        }
        let mut items = self.strip_context_verbs();
        items.extend(TAIL);
        items
    }

    /// The state's context verbs, most relevant first — everything left of the
    /// [`TAIL`]. `held` is a modifier rather than a state of its own: wherever the
    /// hold verb appears it reads `resume` instead of `pause`.
    fn strip_context_verbs(&self) -> Vec<StripItem> {
        let held = self.pace.is_some_and(|p| p.held);
        let hold = if held {
            item("resume", "p", StripVerb::PauseToggle)
        } else {
            item("pause", "⎋", StripVerb::PauseToggle)
        };
        if self.pending_gate.is_some() {
            return vec![
                item("approve", "⏎", StripVerb::ApproveGate),
                item("allow…", "", StripVerb::AllowStage),
                item("abort", "", StripVerb::AbortGate),
            ];
        }
        if self.is_loop_active() {
            // One verb, two labels. `POST /interrupt` stops the loop at the next
            // juncture and withholds the in-flight call's output from the model
            // (docs/control-channel.md) — true in every active state, so the item
            // is always shown. On a foreground bash it *additionally* kills the
            // process group, which is the stronger promise and the one worth
            // naming, so there it reads `kill`. `background` stays bash-only: the
            // handoff adopts a live child into the supervisor, and there is no
            // child to adopt anywhere else.
            let bash_foreground = self.in_flight_call().is_some_and(bash::is_foreground_call);
            return if bash_foreground {
                vec![
                    item("background", "b", StripVerb::Background),
                    item("kill", "⎋⎋", StripVerb::Interrupt),
                    hold,
                ]
            } else {
                vec![item("interrupt", "⎋⎋", StripVerb::Interrupt), hold]
            };
        }
        // Idle. A hold set mid-trajectory outlives the turn, so `resume` is
        // reachable here; there is nothing to pause when nothing is running.
        if held { vec![hold] } else { Vec::new() }
    }

    /// Enter the strip from the composer's `↑`-spill at the 0,0 detent
    /// (docs/tui.md) — focus lands on the leftmost (most relevant) item.
    pub(super) fn focus_strip_from_composer(&mut self) {
        self.composing = false;
        self.strip_focus = Some(0);
        self.ui.debug_message = "controls — ←/→ choose · ⏎ act · ↑ transcript · esc compose".into();
    }

    /// Land on the strip from transcript navigation's `↓` past the last unit —
    /// the symmetric stop on the way back down to the composer.
    pub(super) fn focus_strip_from_transcript(&mut self) {
        self.strip_focus = Some(0);
        // In a read-only view the strip is the bottom of the axis — nothing to
        // fall into below it, so the hint offers only the way back up.
        self.ui.debug_message = if self.read_only() {
            "controls — ←/→ choose · ⏎ act · ↑ transcript".into()
        } else {
            "controls — ←/→ choose · ⏎ act · ↓ compose · esc compose".into()
        };
    }

    /// Fire the verb of item `idx`. Verbs dispatch onto the same mechanisms the
    /// keybindings use; `Detach` raises [`quit_requested`](super::state::UiState)
    /// for the caller (key handler or UI loop) to act on.
    pub(super) fn activate_strip_item(&mut self, idx: usize) {
        let items = self.strip_items();
        let Some(entry) = items.get(idx) else {
            return;
        };
        match entry.verb {
            StripVerb::PauseToggle => self.toggle_pace_hold(),
            StripVerb::PaceWidget => self.enter_pace_widget(),
            StripVerb::Interrupt => self.esc_interrupt(),
            StripVerb::Background => self.background_detach(),
            StripVerb::ApproveGate => self.approve_gate(),
            StripVerb::AllowStage => self.stage_allow(),
            StripVerb::AbortGate => self.abort_gate(),
            StripVerb::Theme => self.open_theme_picker(),
            StripVerb::Help => self.ui_mode = UiMode::Help,
            StripVerb::Detach => self.ui.quit_requested = true,
        }
        self.dirty = true;
    }

    /// The stager class, `allow…` (docs/tui.md): park the operator's draft, move
    /// focus into the composer with the derived grant spec pre-filled for editing
    /// — the bash program for a `bash` gate, empty otherwise (a bare `.allow`
    /// derives a tool grant at commit). The prompt gains the verb's chip; `⏎`
    /// commits, Esc cancels; either way the parked draft comes back.
    pub(super) fn stage_allow(&mut self) {
        let Some(gate) = &self.pending_gate else {
            return;
        };
        let prefill = if gate.name == "bash" {
            gate.input
                .get("command")
                .and_then(|c| c.as_str())
                .and_then(|cmd| cmd.split_whitespace().next())
                .unwrap_or("")
                .to_string()
        } else {
            String::new()
        };
        self.staging = Some(super::state::Staging {
            verb: super::state::StagedVerb::Allow,
            parked: self.composer.text(),
        });
        self.composer.set_text(&prefill);
        self.strip_focus = None;
        self.composing = true;
        self.ui.debug_message = "allow — edit the grant · ⏎ commit · ⎋ cancel".into();
        self.dirty = true;
    }

    /// Commit the staged verb with the composer's current text as its argument,
    /// then restore the parked draft. `Allow` routes through the same `.allow`
    /// submit path the typed form uses (docs/tui.md), so the grant semantics are
    /// one mechanism.
    pub(super) fn commit_staging(&mut self) {
        let Some(staging) = self.staging.take() else {
            return;
        };
        let spec = self.composer.text();
        let submit = match staging.verb {
            super::state::StagedVerb::Allow => {
                let spec = spec.trim();
                if spec.is_empty() {
                    ".allow".to_string()
                } else {
                    format!(".allow {spec}")
                }
            }
        };
        self.submit_composer(submit);
        self.composer.set_text(&staging.parked);
    }

    /// Cancel the staged verb and restore the parked draft — Esc's gentlest rung,
    /// and the path taken when the gate resolves out from under the stager.
    pub(super) fn cancel_staging(&mut self) {
        if let Some(staging) = self.staging.take() {
            self.composer.set_text(&staging.parked);
            self.ui.debug_message = "canceled — draft restored".into();
            self.dirty = true;
        }
    }

    /// The widget class, the pace stepper (docs/tui.md): `⏎` on the `pace` item
    /// enters an inline sub-state where `↑`/`↓` step the between-turns dial in
    /// place. Records the entry speed for revert and keeps focus on the item.
    /// Idempotent — re-entering while active is a no-op (keeps the original entry
    /// value for revert).
    pub(super) fn enter_pace_widget(&mut self) {
        if self.pace_widget.is_some() {
            return;
        }
        self.pace_widget = Some(super::state::PaceWidget {
            entry_speed_ms: self.pace.map(|p| p.speed_ms).unwrap_or(0),
        });
        self.strip_focus = Some(0);
        self.composing = false;
        self.ui.debug_message = "pace — ↑/↓ adjust · ⏎ set · ⎋ revert".into();
        self.dirty = true;
    }

    /// Preview one step of the pace dial while the widget is active — mutate the
    /// live `pace` (so the status bar tracks it) *without* pushing to the daemon;
    /// only [`commit_pace_widget`](Self::commit_pace_widget) sends. `slower` climbs
    /// the ladder toward more between-turns delay.
    pub(super) fn step_pace_widget(&mut self, slower: bool) {
        if self.pace_widget.is_none() {
            return;
        }
        let cur = self.pace.map(|p| p.speed_ms).unwrap_or(0);
        let held = self.pace.is_some_and(|p| p.held);
        let next = super::state::next_pace_rung(cur, slower);
        self.pace = Some(super::PaceView {
            speed_ms: next,
            held,
        });
        self.dirty = true;
    }

    /// Commit the previewed pace: push it to the daemon over the pace channel and
    /// leave the widget (`self.pace` already holds the previewed value). The `⏎`
    /// that ends the stepper.
    pub(super) fn commit_pace_widget(&mut self) {
        if self.pace_widget.take().is_none() {
            return;
        }
        let speed = self.pace.map(|p| p.speed_ms).unwrap_or(0);
        let _ = self.pace_tx.send((Some(speed), None));
        self.ui.debug_message = format!(
            "pace · {}",
            super::state::pace_label(speed, self.pace.is_some_and(|p| p.held))
        )
        .into();
        self.dirty = true;
    }

    /// Revert the stepper to the speed at entry and leave the widget — Esc's
    /// gentlest rung (the previews were never sent, so nothing to unsend on the
    /// daemon; just restore the local value the status bar reflects).
    pub(super) fn revert_pace_widget(&mut self) {
        if let Some(widget) = self.pace_widget.take() {
            let held = self.pace.is_some_and(|p| p.held);
            self.pace = Some(super::PaceView {
                speed_ms: widget.entry_speed_ms,
                held,
            });
            self.ui.debug_message = "pace reverted".into();
            self.dirty = true;
        }
    }

    /// Render the strip row: one relevance-ordered list left to right, cut at the
    /// first item that doesn't fit — so the omnipresent tail, being rightmost, is
    /// what a narrow terminal loses. The focused item is marked with `▸` in its
    /// reserved marker cell. A dim contextual note (redirect hint at a gate,
    /// commit/cancel hint while staging) is right-aligned and reserved *out of* the
    /// item budget, so an instruction is never the thing that gets cut. Rebuilds
    /// [`strip_hits`](InteractiveState::strip_hits), the column-range → item map
    /// mouse clicks resolve against.
    pub(super) fn render_control_strip(&mut self, buf: &mut Buffer, area: Rect) {
        let items = self.strip_items();
        // Clamp focus to the current set (a state transition may have shrunk it).
        if let Some(i) = self.strip_focus
            && i >= items.len()
        {
            self.strip_focus = Some(items.len().saturating_sub(1));
        }

        for x in 0..area.width {
            if let Some(cell) = buf.cell_mut((area.x + x, area.y)) {
                cell.reset();
                cell.set_style(self.theme.document_bg_style);
            }
        }
        self.strip_hits.clear();

        // The note's width comes off the item budget before any item is placed, so
        // an instruction is never what the cut drops; it is then drawn *adjacent*
        // to the last item that fit, where it reads as being about the row rather
        // than about the right edge.
        let note = self.strip_note();
        let reserved = note.map_or(0, |text| text.chars().count() as u16 + 2);
        let budget = area.width.saturating_sub(reserved);

        let mut x: u16 = 1;
        for (i, entry) in items.iter().enumerate() {
            if x + self.strip_item_width(entry) > budget {
                break; // fit-to-width: the lowest-ranked items drop first
            }
            x = self.render_strip_item(buf, area, x, entry, i) + 2;
        }

        if let Some(text) = note {
            put_row(buf, area, x, text, self.theme.muted_style);
        }
    }

    /// The dim instruction for a state that has one — always a sentence about the
    /// keys the *current* state binds, never a verb the operator picks.
    fn strip_note(&self) -> Option<&'static str> {
        if self.pace_widget.is_some() {
            Some("↑/↓ adjust · ⏎ set · ⎋ revert")
        } else if self.staging.is_some() {
            Some("draft saved · ⏎ commit · ⎋ cancel")
        } else if self.pending_gate.is_some() {
            Some("type below to redirect")
        } else {
            None
        }
    }

    /// One item's cells: `▸ label hint ` — the reserved marker cell (`▸` focused,
    /// space otherwise) then the label, then the dim fast-path hint. Records the
    /// column range in [`strip_hits`](InteractiveState::strip_hits).
    fn render_strip_item(
        &mut self,
        buf: &mut Buffer,
        area: Rect,
        x: u16,
        entry: &StripItem,
        i: usize,
    ) -> u16 {
        let sel = self.strip_focus == Some(i);
        let (body, hint) = if sel {
            (
                self.theme.strip_item_focused_style,
                self.theme.strip_hint_focused_style,
            )
        } else if is_tail(entry) {
            // The tail reads dimmer than the context verbs: same row, lower
            // priority — and the same ordering the cut-from-right rule follows.
            (self.theme.strip_tail_style, self.theme.strip_tail_style)
        } else {
            (self.theme.strip_item_style, self.theme.strip_hint_style)
        };
        let marker = if sel { "▸ " } else { "  " };
        let label = self.strip_item_label(entry);
        let mut col = put_row(buf, area, x, marker, body);
        col = put_row(buf, area, col, &label, body);
        if !entry.hint.is_empty() {
            col = put_row(buf, area, col, " ", body);
            col = put_row(buf, area, col, entry.hint, hint);
        }
        col = put_row(buf, area, col, " ", body);
        self.strip_hits.push((x, col, i));
        col
    }

    /// One item's label as rendered. The pace item's is the live value (`pace 2s`,
    /// or `▴ pace 2s ▾` while the stepper is active — the `▴`/`▾` are the
    /// grid-visible "stepping" tell, the way `▸` marks focus, docs/tui.md); every
    /// other item's is its static text.
    fn strip_item_label(&self, entry: &StripItem) -> String {
        if entry.verb != StripVerb::PaceWidget {
            return entry.label.to_string();
        }
        let val = super::state::pace_label(self.pace.map(|p| p.speed_ms).unwrap_or(0), false);
        if self.pace_widget.is_some() {
            format!("▴ pace {val} ▾")
        } else {
            format!("pace {val}")
        }
    }

    /// Rendered width of one item: marker cell + gap, label, hint, trailing pad.
    /// Shares [`strip_item_label`](Self::strip_item_label) with the renderer, so
    /// the fit test and the recorded hit range can't drift from what's drawn.
    fn strip_item_width(&self, entry: &StripItem) -> u16 {
        let hint_w = if entry.hint.is_empty() {
            0
        } else {
            1 + entry.hint.chars().count()
        };
        (2 + self.strip_item_label(entry).chars().count() + hint_w + 1) as u16
    }
}

/// Whether an item belongs to the omnipresent [`TAIL`] — the dim treatment and the
/// cut-first ordering are the same membership, read off the verb rather than
/// carried on the item.
fn is_tail(entry: &StripItem) -> bool {
    TAIL.iter().any(|t| t.verb == entry.verb)
}

/// Write `text` at column `col` of the strip row, truncating at the area edge;
/// returns the next free column. Column-at-a-time like the other chrome renders.
fn put_row(
    buf: &mut Buffer,
    area: Rect,
    col: u16,
    text: &str,
    style: ratatui::style::Style,
) -> u16 {
    let mut x = col;
    for ch in text.chars() {
        if x >= area.width {
            break;
        }
        if let Some(cell) = buf.cell_mut((area.x + x, area.y)) {
            cell.set_char(ch).set_style(style);
        }
        x += 1;
    }
    x
}

#[cfg(test)]
mod test;
