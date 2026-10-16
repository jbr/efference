//! The control strip (docs/tui.md): a one-row, always-present surface between the
//! transcript and the composer, carrying the loop verbs the state ranks as focusable,
//! clickable, button-like items. It is chrome — never projected, never logged — built
//! from the `state` topics plus the committed log.
//!
//! Two things shape everything below. The item list is **one relevance-ordered
//! sequence per state** — the state's context verbs, then the omnipresent tail —
//! filled to the available width and cut from the right. And **focus, the click
//! hit-map, and the pin all hold a [`StripVerb`], never an index**: the list is
//! rebuilt per frame with the context verbs *prepended*, so every position shifts
//! when the state flips, and an index-held focus would become a different verb
//! between the aim and the press.

use ratatui::{buffer::Buffer, layout::Rect, style::Style};

use super::{InteractiveState, UiMode};
use crate::bash;

/// A verb the strip can carry — and the thing strip focus, a click target, and a
/// sidebar return all hold, in place of a position (docs/tui.md). Dispatch happens
/// in [`activate_strip_verb`](InteractiveState::activate_strip_verb), always onto
/// mechanisms that also exist without the strip (pace, interrupt, gate approval).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StripVerb {
    /// Toggle the pace hold (pause/resume).
    PauseToggle,
    /// Enter the inline pace stepper (the widget class, docs/tui.md).
    PaceWidget,
    /// Interrupt the loop (`POST /loop/interrupt`) — shown as `kill` where it also
    /// kills a process group, `interrupt` everywhere else (docs/tui.md).
    Interrupt,
    /// Foreground→background handoff (`PATCH /processes/:id {background: true}`).
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
    /// Open the session switcher (`sessions.rs`): the host's sessions, with
    /// switch and end.
    Sessions,
    /// Leave the TUI; the daemon and session keep running.
    Detach,
    /// Try a failed start again (`starting.rs`): resume it again, or reopen the
    /// new-session form for a creation.
    Retry,
}

/// One strip item: a label and the fast-path key it teaches (empty when there is
/// none). A bare-letter hint is also the item's **scoped mnemonic** — it fires the
/// verb while the strip is focused, and only where the item is shown (docs/tui.md).
#[derive(Debug, Clone, Copy)]
pub(crate) struct StripItem {
    pub label: &'static str,
    pub hint: &'static str,
    pub verb: StripVerb,
}

const fn item(label: &'static str, hint: &'static str, verb: StripVerb) -> StripItem {
    StripItem { label, hint, verb }
}

/// The omnipresent tail, appended to every state's context verbs and therefore
/// the first thing a narrow width cuts (right to left: `detach`, `help`, `theme`,
/// `sessions`, `pace`). Ordered least-cuttable first: `pace` is a control the
/// operator reaches for mid-run, `detach` is the one with the plainest keyboard
/// fallback. `sessions` sits second because it is the doorway to the verbs that
/// act on a session's life rather than its loop — switch, and end (`sessions.rs`).
const TAIL: [StripItem; 5] = [
    item("pace", "", StripVerb::PaceWidget),
    item("sessions", "s", StripVerb::Sessions),
    item("theme", "t", StripVerb::Theme),
    item("help", "?", StripVerb::Help),
    item("detach", "q", StripVerb::Detach),
];

/// The whole strip in a read-only view (docs/tui.md — `efference dev view`): the tail
/// verbs that act on the client alone, with `detach` reading `close` because there
/// is no daemon to leave running. `pace` is dropped with the rest of the loop verbs
/// — it is a control over a running loop, and there isn't one.
const VIEW_TAIL: [StripItem; 3] = [
    item("theme", "t", StripVerb::Theme),
    item("help", "?", StripVerb::Help),
    item("close", "q", StripVerb::Detach),
];

impl InteractiveState<'_> {
    /// The item list as drawn: the state's ranking, or the pinned order while the
    /// strip is engaged. Re-evaluated per frame; unengaged, it changes only when the
    /// underlying state does, and engaged it does not change at all
    /// ([`sync_strip_engagement`](Self::sync_strip_engagement)).
    pub(super) fn strip_items(&self) -> Vec<StripItem> {
        let live = self.strip_live_items();
        let Some(pinned) = &self.strip_pinned else {
            return live;
        };
        // Frozen while engaged: the pinned *order* is what the operator's hands and
        // eyes are aimed at, so it holds. Each slot's label is re-read from the live
        // list, because a slot whose verb still applies must say what it currently
        // does; a slot whose verb has gone keeps its last label and greys (below).
        pinned
            .iter()
            .map(|held| {
                live.iter()
                    .find(|item| item.verb == held.verb)
                    .copied()
                    .unwrap_or(*held)
            })
            .collect()
    }

    /// The list the current state ranks, before any freeze. This is what
    /// applicability is read from: an item on the row is dispatchable exactly when
    /// its verb appears here.
    pub(super) fn strip_live_items(&self) -> Vec<StripItem> {
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
        if self.owns_host
            && let Some(leave) = items.iter_mut().find(|i| i.verb == StripVerb::Detach)
        {
            // Nothing outlives this client, so there is nothing to detach from.
            leave.label = "quit";
        }
        items
    }

    /// Whether `verb` is dispatchable right now — i.e. the state still ranks it.
    /// A pinned slot whose verb fails this renders greyed and fires nothing.
    pub(super) fn strip_verb_applies(&self, verb: StripVerb) -> bool {
        self.strip_live_items().iter().any(|item| item.verb == verb)
    }

    /// The verb `↑` from the composer lands on: the leftmost, most relevant item.
    pub(super) fn leading_strip_verb(&self) -> Option<StripVerb> {
        self.strip_items().first().map(|item| item.verb)
    }

    /// Where the focused verb currently sits, for the `←`/`→` walk. Re-derived
    /// each time rather than stored: the stored form is the verb.
    fn strip_focus_index(&self) -> Option<usize> {
        let focused = self.strip_focus?;
        self.strip_items()
            .iter()
            .position(|item| item.verb == focused)
    }

    /// Move the strip selection one item, clamped at both ends. Identity in,
    /// identity out — the index exists only for the duration of the step.
    pub(super) fn move_strip_focus(&mut self, forward: bool) {
        let items = self.strip_items();
        if items.is_empty() {
            return;
        }
        let idx = self.strip_focus_index().unwrap_or(0);
        let next = if forward {
            (idx + 1).min(items.len() - 1)
        } else {
            idx.saturating_sub(1)
        };
        self.strip_focus = items.get(next).map(|item| item.verb);
    }

    /// Whether the operator is *on* the strip — keyboard focus, or the mouse over
    /// its row. Either engagement pins the list; it re-ranks when both are gone.
    fn strip_engaged(&self) -> bool {
        self.strip_focus.is_some() || self.viewport.strip_hovered
    }

    /// Pin the item list on engagement, release it when engagement ends. Called
    /// once per frame from the renderer, so every path that moves focus or the
    /// mouse — key, click, Esc, sidebar, a daemon-side state change — is covered
    /// by one rule rather than by a set-and-clear at each site.
    ///
    /// The two sub-states that own the row outright (the pace stepper, staging)
    /// never freeze: their row is a single widget, not a ranked list, and holding a
    /// pre-widget list under it would render items the sub-state has taken away.
    fn sync_strip_engagement(&mut self) {
        if self.pace_widget.is_some() || self.staging.is_some() || !self.strip_engaged() {
            self.strip_pinned = None;
        } else if self.strip_pinned.is_none() {
            self.strip_pinned = Some(self.strip_live_items());
        }
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
        if self.start_failed() {
            return vec![item("retry", "r", StripVerb::Retry)];
        }
        if self.pending_gate.is_some() {
            return vec![
                item("approve", "⏎", StripVerb::ApproveGate),
                item("allow…", "", StripVerb::AllowStage),
                item("abort", "", StripVerb::AbortGate),
            ];
        }
        if self.is_loop_active() {
            // One verb, two labels. `POST /loop/interrupt` stops the loop at the next
            // juncture and withholds the in-flight call's output from the model
            // (docs/control-channel.md) — true in every active state, so the item
            // is always shown. On a foreground bash it *additionally* kills the
            // process group, which is the stronger promise and the one worth
            // naming, so there it reads `kill`. `background` stays bash-only: the
            // handoff adopts a live child into the process monitor, and there is no
            // child to adopt anywhere else.
            let bash_foreground = self
                .in_flight_call()
                .is_some_and(|call| bash::is_foreground_call(&call));
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
        self.strip_focus = self.leading_strip_verb();
        self.ui.debug_message = "controls — ←/→ choose · ⏎ act · ↑ transcript · esc compose".into();
    }

    /// Land on the strip from transcript navigation's `↓` past the last unit —
    /// the symmetric stop on the way back down to the composer.
    pub(super) fn focus_strip_from_transcript(&mut self) {
        self.strip_focus = self.leading_strip_verb();
        // In a read-only view the strip is the bottom of the axis — nothing to
        // fall into below it, so the hint offers only the way back up.
        self.ui.debug_message = if self.read_only() {
            "controls — ←/→ choose · ⏎ act · ↑ transcript".into()
        } else {
            "controls — ←/→ choose · ⏎ act · ↓ compose · esc compose".into()
        };
    }

    /// Fire `verb`. Verbs dispatch onto the same mechanisms the keybindings use;
    /// `Detach` raises [`quit_requested`](super::state::UiState) for the caller
    /// (key handler or UI loop) to act on.
    ///
    /// A verb the state has stopped ranking fires **nothing** and says so, rather
    /// than falling through to whatever verb occupies its slot.
    pub(super) fn activate_strip_verb(&mut self, verb: StripVerb) {
        if !self.strip_verb_applies(verb) {
            self.ui.debug_message = "that verb no longer applies here".into();
            self.dirty = true;
            return;
        }
        match verb {
            StripVerb::PauseToggle => self.toggle_pace_hold(),
            StripVerb::PaceWidget => self.enter_pace_widget(),
            StripVerb::Interrupt => self.esc_interrupt(),
            StripVerb::Background => self.background_detach(),
            StripVerb::ApproveGate => self.approve_gate(),
            StripVerb::AllowStage => self.stage_allow(),
            StripVerb::AbortGate => self.abort_gate(),
            StripVerb::Sessions => self.open_sessions(),
            StripVerb::Theme => self.open_theme_picker(),
            StripVerb::Help => self.ui_mode = UiMode::Help,
            StripVerb::Detach => self.ui.quit_requested = true,
            StripVerb::Retry => self.retry_start(),
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
        self.strip_pinned = None;
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
        // The stepper owns the row, so nothing pinned survives it.
        self.strip_pinned = None;
        self.strip_focus = Some(StripVerb::PaceWidget);
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
        // One place decides whether the list is pinned, and it runs before the list
        // is read — so what is drawn, what a click resolves against, and what a key
        // dispatches on are the same list.
        self.sync_strip_engagement();
        let items = self.strip_items();
        // A focused verb the sub-states took away (the pace stepper, staging) has no
        // slot to grey in; it drops rather than pointing at nothing.
        if let Some(verb) = self.strip_focus
            && !items.iter().any(|item| item.verb == verb)
        {
            self.strip_focus = items.first().map(|item| item.verb);
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
        for entry in &items {
            if x + self.strip_item_width(entry) > budget {
                break; // fit-to-width: the lowest-ranked items drop first
            }
            x = self.render_strip_item(buf, area, x, entry) + 2;
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
        } else if self.strip_list_is_stale() {
            // Leads the gate's own note deliberately: a pinned row that has diverged
            // from the state is the one thing the operator must not act on
            // unknowingly (docs/tui.md).
            Some("pinned · leave to re-rank")
        } else if self.pending_gate.is_some() {
            Some("type below to redirect")
        } else if self.clipboard_image {
            // A terminal's own paste cannot carry an image; `C-v` reads the clipboard itself.
            Some("image in clipboard · C-v to attach")
        } else {
            None
        }
    }

    /// Whether the pinned row has diverged from what the state now ranks — a verb
    /// greyed out in place, or one that became relevant and is being held back.
    /// This is what the frozen row has to *say*, since a freeze nobody can see is
    /// just stale information.
    fn strip_list_is_stale(&self) -> bool {
        let Some(pinned) = &self.strip_pinned else {
            return false;
        };
        let live = self.strip_live_items();
        live.len() != pinned.len()
            || live
                .iter()
                .zip(pinned)
                .any(|(live, held)| live.verb != held.verb)
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
    ) -> u16 {
        let sel = self.strip_focus == Some(entry.verb);
        // A pinned slot whose verb the state stopped ranking greys **in place** —
        // the position is what the freeze protects, so removing it would be the
        // shift the freeze exists to prevent. Greying is also the tell: it takes
        // precedence over the focus surface, because "this fires nothing" is what
        // the operator needs to read before they press.
        let (body, hint) = if !self.strip_verb_applies(entry.verb) {
            (
                self.theme.strip_item_disabled_style,
                self.theme.strip_item_disabled_style,
            )
        } else if sel {
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
        // Under the pointer, the button's surface lifts a step toward the text
        // colour, so a click target reads as one before it is pressed. Focus keeps
        // its own surface; a greyed slot fires nothing and does not lift.
        let (body, hint) = if !sel
            && self.strip_verb_applies(entry.verb)
            && self.viewport.strip_hover == Some(entry.verb)
        {
            let lift = |style: Style| match (style.bg, self.theme.status_style.fg) {
                (Some(bg), Some(fg)) => style.bg(super::theme::mix(bg, fg, 0.18)),
                _ => style,
            };
            (lift(body), lift(hint))
        } else {
            (body, hint)
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
        // Screen columns: the strip may sit in a centred column (docs/tui.md).
        self.strip_hits.push((area.x + x, area.x + col, entry.verb));
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
            cell.set_char(super::write_text::fold_control(ch))
                .set_style(style);
        }
        x += 1;
    }
    x
}

#[cfg(test)]
mod test;
