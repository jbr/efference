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

impl InteractiveState<'_> {
    /// The ranked item list for the current state. Re-evaluated per frame;
    /// contents only change when the underlying state does, so re-ranking happens
    /// at state transitions and never while the strip sits stable.
    ///
    /// Two transient sub-states own the row outright and so carry no tail — their
    /// `⏎`/`⎋` belong to the sub-state, and a clickable tail beside them would
    /// invite acting out from under it.
    pub(super) fn strip_items(&self) -> Vec<StripItem> {
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
        self.ui.debug_message = "controls — ←/→ choose · ⏎ act · ↓ compose · esc compose".into();
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
mod scenarios {
    use crate::tui::TuiEvent;
    use crate::tui::harness::Screenplay;
    use crossterm::event::{KeyCode, KeyModifiers};

    const NONE: KeyModifiers = KeyModifiers::NONE;

    /// The item labels in rank order — the strip's contents model, asserted
    /// directly. (The status bar's hint text mentions some of the same words, so
    /// `find` can't carry a negative claim about the strip.)
    fn labels(play: &Screenplay) -> Vec<&'static str> {
        play.state.strip_items().iter().map(|e| e.label).collect()
    }

    /// The strip is always present and never empty: an idle session has no context
    /// verbs at all, so what shows is exactly the omnipresent tail, in tail order,
    /// directly above the composer.
    #[test]
    fn strip_is_always_present_and_never_empty() {
        let mut play = Screenplay::new(80, 10);
        play.user("hello");
        assert_eq!(
            labels(&play),
            vec!["pace", "theme", "help", "detach"],
            "idle is the tail alone"
        );
        let (px, _) = play.find("pace").expect("pace always shows");
        let (dx, _) = play.find("detach").expect("detach is in the tail");
        assert!(px < dx, "the tail runs most- to least-protected");
        play.snapshot("strip_idle_set");
    }

    /// The context verbs swap with the state while the tail stays put — the tail is
    /// omnipresent, so a state change never costs the operator `pace`/`theme`/
    /// `help`/`detach`.
    #[test]
    fn strip_context_verbs_swap_over_a_fixed_tail() {
        let mut play = Screenplay::new(80, 12);
        play.user("run it");
        assert!(
            !labels(&play).contains(&"pause"),
            "no loop verbs while idle"
        );

        play.running_bash("toolu_b", "cargo test");
        assert_eq!(
            labels(&play),
            vec![
                "background",
                "kill",
                "pause",
                "pace",
                "theme",
                "help",
                "detach"
            ],
            "the foreground-bash verbs lead, the tail follows unchanged"
        );
        play.snapshot("strip_running_bash");
    }

    /// `background` is foreground-bash-only — it adopts a live child into the
    /// supervisor, and there is no child to adopt anywhere else. The interrupt is
    /// not: it stops the loop and withholds the call's output whatever the tool
    /// (docs/control-channel.md), so it stays, reading `interrupt` where it isn't
    /// also killing a process group.
    #[test]
    fn strip_offers_background_only_for_foreground_bash_but_always_an_interrupt() {
        use serde_json::json;
        let mut play = Screenplay::new(80, 12);
        play.user("read it");

        play.running_tool("toolu_r", "read", json!({ "path": "src/log.rs" }));
        assert_eq!(
            labels(&play),
            vec!["interrupt", "pause", "pace", "theme", "help", "detach"],
            "a non-bash tool still offers the interrupt, without the handoff"
        );

        // A *backgrounded* bash call returns at once and runs under the supervisor
        // (docs/supervisor.md) — it is never the foreground tool either.
        play.running_tool(
            "toolu_g",
            crate::bash::NAME,
            json!({ "command": "cargo watch", "background": true }),
        );
        assert!(
            !labels(&play).contains(&"background"),
            "an already-backgrounded call offers no handoff"
        );
    }

    /// The non-bash `interrupt` fires the same `POST /interrupt` the bash `kill`
    /// does — one endpoint, two labels for the two promises it keeps. Before the
    /// latch this item would have been a button that did nothing: nothing awaits
    /// the abort future during an MCP call, so the notify had nowhere to land.
    #[test]
    fn strip_interrupt_on_a_non_bash_tool_fires_the_interrupt() {
        use serde_json::json;
        let mut play = Screenplay::new(80, 12);
        play.user("read it");
        play.running_tool("toolu_r", "read", json!({ "path": "src/log.rs" }));

        play.route_key(KeyCode::Up, NONE); // → strip, landing on `interrupt`
        play.route_key(KeyCode::Enter, NONE);
        assert!(
            play.took_interrupt(),
            "⏎ on interrupt posts the interrupt for a tool it cannot kill"
        );
    }

    /// While the model is generating — running, but nothing called yet — there is
    /// no process to kill and no child to hand off, but the interrupt still
    /// applies: it latches, and the call the model is about to make never runs
    /// (docs/control-channel.md).
    #[test]
    fn strip_while_generating_offers_the_interrupt_and_the_hold() {
        let mut play = Screenplay::new(80, 12);
        play.user("think about it");
        play.event(TuiEvent::Running(true));
        assert_eq!(
            labels(&play),
            vec!["interrupt", "pause", "pace", "theme", "help", "detach"]
        );
    }

    /// `held` is a modifier, not a state: wherever the hold verb appears it reads
    /// `resume`, including at idle — a hold set mid-trajectory outlives the turn,
    /// so the way out of it stays reachable.
    #[test]
    fn strip_hold_reads_resume_and_survives_into_idle() {
        let mut play = Screenplay::new(80, 12);
        play.user("go");
        play.running_bash("toolu_b", "cargo test");
        play.event(TuiEvent::Pace(super::super::PaceView {
            speed_ms: 0,
            held: true,
        }));
        assert!(labels(&play).contains(&"resume"), "held renames the verb");
        assert!(!labels(&play).contains(&"pause"));

        play.event(TuiEvent::Running(false));
        assert_eq!(
            labels(&play),
            vec!["resume", "pace", "theme", "help", "detach"],
            "resume is still reachable once the turn ends"
        );
    }

    /// Fit-to-width cuts from the right, so a narrow terminal loses the tail's
    /// least-relevant end first and never a context verb.
    #[test]
    fn strip_cuts_from_the_right_when_narrow() {
        let mut wide = Screenplay::new(120, 12);
        wide.user("go");
        wide.running_bash("toolu_b", "cargo test");
        assert!(
            wide.find("detach").is_some(),
            "a wide terminal fits the whole list"
        );

        let mut narrow = Screenplay::new(40, 12);
        narrow.user("go");
        narrow.running_bash("toolu_b", "cargo test");
        assert!(
            narrow.find("background").is_some(),
            "the most relevant verb survives the cut"
        );
        assert!(
            narrow.find("detach").is_none(),
            "the tail's far end is what's lost"
        );
    }

    /// `↑` from the composer lands on the strip's leftmost (most relevant) item,
    /// marked with `▸` in the reserved marker cell; `→`/`←` move between items and
    /// clamp at the ends — and focus movement never shifts the labels' columns.
    #[test]
    fn strip_focus_marker_moves_without_shifting_text() {
        let mut play = Screenplay::new(80, 12);
        play.user("go");
        play.running_bash("toolu_b", "cargo test");
        let pause_before = play.find("pause").expect("pause on screen");

        play.route_key(KeyCode::Up, NONE);
        assert_eq!(play.state.strip_focus, Some(0), "↑ lands on the first item");
        let m1 = play.find("▸").expect("the focused item shows the marker");
        assert!(m1.0 < play.find("background").unwrap().0);

        play.route_key(KeyCode::Right, NONE);
        let m2 = play.find("▸").expect("marker follows focus");
        assert!(m2.0 > m1.0, "→ moved the marker right");
        assert_eq!(
            play.find("pause").unwrap(),
            pause_before,
            "focus movement does not shift item text"
        );

        play.route_key(KeyCode::Left, NONE);
        play.route_key(KeyCode::Left, NONE);
        assert_eq!(play.state.strip_focus, Some(0), "← clamps at the left end");
    }

    /// `⏎` on the focused item fires its verb through the same mechanism as the
    /// keybinding: pause holds the loop (status bar shows HELD) and the strip's
    /// set flips to the held family, with focus landing on `resume`.
    #[test]
    fn strip_enter_fires_the_focused_verb() {
        let mut play = Screenplay::new(80, 12);
        play.user("go");
        play.event(TuiEvent::Running(true));
        play.route_key(KeyCode::Up, NONE); // → strip, on `interrupt` (generating: the interrupt leads)
        play.route_key(KeyCode::Right, NONE); // → `pause`
        play.route_key(KeyCode::Enter, NONE);
        assert!(play.find("HELD").is_some(), "⏎ on pause holds the loop");
        assert!(
            play.find("resume").is_some(),
            "the set flipped to the held family"
        );
        assert_eq!(
            play.state.strip_focus,
            Some(1),
            "focus stays on its slot, whose verb is now resume"
        );
    }

    /// The focused item's surface color differs from an unfocused item's — the
    /// glyph marker carries focus in the grid, the raised surface carries it in
    /// pixels; the grid can't show the latter, so `cell` asserts it.
    #[test]
    fn strip_focused_item_has_a_raised_surface() {
        let mut play = Screenplay::new(80, 10);
        play.user("hello");
        play.route_key(KeyCode::Up, NONE); // → strip, on `pace`
        play.route_key(KeyCode::Right, NONE); // focus `theme`
        let (tx, ty) = play.find("theme").expect("theme on screen");
        let (hx, hy) = play.find("help").expect("help on screen");
        let focused = play.cell(tx, ty);
        let unfocused = play.cell(hx, hy);
        assert_ne!(
            focused.bg, unfocused.bg,
            "the focused item's surface is raised: {focused:?} vs {unfocused:?}"
        );
        assert_ne!(
            focused.bg,
            play.cell(0, ty).bg,
            "an item's surface differs from the document background"
        );
    }

    /// `⏎` on `detach` leaves the TUI; the daemon and session keep running, which
    /// is why it is one verb and not a detach/terminate pair. It sits at the tail's
    /// far end — least relevant, first cut, and reachable by keyboard.
    #[test]
    fn strip_detach_quits() {
        let mut play = Screenplay::new(80, 10);
        play.user("hello");
        play.route_key(KeyCode::Up, NONE); // idle: pace · theme · help · detach
        play.route_key(KeyCode::Right, NONE);
        play.route_key(KeyCode::Right, NONE);
        play.route_key(KeyCode::Right, NONE);
        assert!(
            play.key_quits(KeyCode::Enter, NONE),
            "⏎ on detach ends the UI loop"
        );
    }

    /// A pending gate populates the strip with its full verb set — `approve` on
    /// the landing slot (`↑ ⏎` approves), `allow…`, `abort` — plus the dim
    /// redirect note.
    #[test]
    fn strip_gate_set_leads_with_approve() {
        use serde_json::json;
        let mut play = Screenplay::new(80, 14);
        play.user("clean up");
        play.pending_gate("toolu_g", "bash", json!({ "command": "rm -rf ./build" }));
        assert_eq!(
            labels(&play),
            vec![
                "approve", "allow…", "abort", "pace", "theme", "help", "detach"
            ],
            "the gate verbs in rank order, then the tail"
        );
        assert!(
            play.find("type below to redirect").is_some(),
            "the strip carries the redirect note"
        );

        play.route_key(KeyCode::Up, NONE);
        play.route_key(KeyCode::Enter, NONE);
        assert!(
            play.state.pending_gate.is_none(),
            "⏎ on approve resolved the gate"
        );
        assert!(
            matches!(
                play.took_approval(),
                Some((id, crate::frontend::Approval::Approve)) if id == "toolu_g"
            ),
            "…with an Approve decision"
        );
    }

    /// `abort` sends the same decision the typed `.abort` sentinel produces, and
    /// leaves any drafted text in place.
    #[test]
    fn strip_abort_sends_abort_and_keeps_the_draft() {
        use serde_json::json;
        let mut play = Screenplay::new(80, 14);
        play.user("clean up");
        play.type_str("actually wait");
        play.pending_gate("toolu_g", "bash", json!({ "command": "rm -rf ./build" }));
        play.route_key(KeyCode::Up, NONE); // to the 0,0 detent of the draft
        play.route_key(KeyCode::Up, NONE); // spill → strip: approve
        play.route_key(KeyCode::Right, NONE); // allow…
        play.route_key(KeyCode::Right, NONE); // abort
        play.route_key(KeyCode::Enter, NONE);
        assert!(
            matches!(
                play.took_approval(),
                Some((_, crate::frontend::Approval::Abort))
            ),
            "⏎ on abort sends Abort"
        );
        assert_eq!(
            play.state.composer.text(),
            "actually wait",
            "the draft survives an abort from the strip"
        );
    }

    /// The stager class end to end (docs/tui.md): activating `allow…` parks the
    /// draft, pre-fills the derived grant spec (the bash program) in the composer
    /// under an `allow` chip, `⏎` commits a ConfigEdit through the `.allow` path,
    /// and the parked draft comes back.
    #[test]
    fn allow_stager_prefills_commits_and_restores_the_draft() {
        use crate::frontend::Approval;
        use crate::log::ConfigChange;
        use serde_json::json;

        let mut play = Screenplay::new(80, 14);
        play.user("show the log");
        play.type_str("a half-typed thought");
        play.pending_gate("toolu_g", "bash", json!({ "command": "git log --oneline" }));

        play.route_key(KeyCode::Up, NONE); // to the 0,0 detent of the draft
        play.route_key(KeyCode::Up, NONE); // spill → strip: approve
        play.route_key(KeyCode::Right, NONE); // allow…
        play.route_key(KeyCode::Enter, NONE); // stage
        assert!(play.state.staging.is_some(), "allow… enters staging");
        assert!(play.state.composing, "staging edits in the composer");
        assert_eq!(
            play.state.composer.text(),
            "git",
            "the derived spec (the bash program) is pre-filled for editing"
        );
        assert!(
            play.find("draft saved · ⏎ commit · ⎋ cancel").is_some(),
            "the strip note explains the staging state"
        );

        play.type_str(" log"); // widen the prefix: `git` → `git log`
        play.route_key(KeyCode::Enter, NONE); // commit
        match play.took_approval() {
            Some((id, Approval::ConfigEdit { change, .. })) => {
                assert_eq!(id, "toolu_g");
                assert_eq!(
                    change,
                    ConfigChange::Command {
                        command: "git log".into()
                    },
                    "the edited spec became the command grant"
                );
            }
            other => panic!("expected a ConfigEdit decision, got {other:?}"),
        }
        assert!(play.state.staging.is_none(), "staging ends on commit");
        assert!(play.state.pending_gate.is_none(), "the gate resolved");
        assert_eq!(
            play.state.composer.text(),
            "a half-typed thought",
            "the parked draft is restored after commit"
        );
    }

    /// Esc during staging cancels: the parked draft returns, the gate is still
    /// pending, and no decision was sent.
    #[test]
    fn allow_stager_esc_cancels_and_restores() {
        use serde_json::json;
        let mut play = Screenplay::new(80, 14);
        play.user("show the log");
        play.type_str("draft in progress");
        play.pending_gate("toolu_g", "bash", json!({ "command": "git log" }));
        play.route_key(KeyCode::Up, NONE); // detent
        play.route_key(KeyCode::Up, NONE); // spill → strip
        play.route_key(KeyCode::Right, NONE);
        play.route_key(KeyCode::Enter, NONE); // stage
        assert_eq!(play.state.composer.text(), "git");

        play.route_key(KeyCode::Esc, NONE); // cancel — the gentlest Esc rung
        assert!(play.state.staging.is_none(), "esc ends staging");
        assert_eq!(
            play.state.composer.text(),
            "draft in progress",
            "the parked draft is restored on cancel"
        );
        assert!(
            play.state.pending_gate.is_some(),
            "the gate is still pending"
        );
        assert!(play.took_approval().is_none(), "no decision was sent");
    }

    /// A gate that resolves out from under the stager (another client, a
    /// daemon-side abort) cancels the staging rather than letting its commit fall
    /// through to a normal turn submit.
    #[test]
    fn gate_resolving_elsewhere_cancels_staging() {
        use serde_json::json;
        let mut play = Screenplay::new(80, 14);
        play.user("show the log");
        play.type_str("my draft");
        play.pending_gate("toolu_g", "bash", json!({ "command": "git log" }));
        play.route_key(KeyCode::Up, NONE); // detent
        play.route_key(KeyCode::Up, NONE); // spill → strip
        play.route_key(KeyCode::Right, NONE);
        play.route_key(KeyCode::Enter, NONE); // stage
        assert!(play.state.staging.is_some());

        play.event(TuiEvent::Gate(None)); // resolved elsewhere
        assert!(
            play.state.staging.is_none(),
            "staging cancels when the gate goes away"
        );
        assert_eq!(
            play.state.composer.text(),
            "my draft",
            "the parked draft is restored"
        );
    }

    /// The staging composer renders the verb's chip at the prompt — the visible
    /// "your next ⏎ commits an argument, not a message" signal.
    #[test]
    fn staging_renders_the_chip_and_prefill() {
        use serde_json::json;
        let mut play = Screenplay::new(80, 14);
        play.user("clean up");
        play.pending_gate("toolu_g", "bash", json!({ "command": "rm -rf ./build" }));
        play.route_key(KeyCode::Up, NONE);
        play.route_key(KeyCode::Right, NONE);
        play.route_key(KeyCode::Enter, NONE); // stage
        play.snapshot("gate_allow_staging");
    }

    /// Rendering records the items' column ranges for mouse hit-testing.
    #[test]
    fn strip_hits_are_recorded_for_clicks() {
        let mut play = Screenplay::new(80, 10);
        play.user("hello");
        play.render();
        assert!(
            play.state.strip_hits.len() >= 3,
            "idle set + universal recorded: {:?}",
            play.state.strip_hits
        );
    }
}
