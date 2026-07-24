//! The control strip (docs/tui.md): a one-row, always-present surface between the
//! transcript and the composer, showing the currently relevant loop verbs as
//! focusable, clickable, button-like items. It is the discoverable slow path and
//! the keybinding legend in one place — each item carries the key that would fire
//! the same verb without visiting the strip.
//!
//! Items are ranked left→right by relevance for the current state family (gate
//! pending / held / running / idle), so `↑ ⏎` from the composer always lands on
//! the most relevant verb. Universal items (`detach`) are pinned at the right
//! edge, dimmer, at a stable position. Contextual items that don't fit the width
//! are dropped from the right (lowest relevance first); the universals always
//! render. The strip is chrome, not transcript: its contents come from the same
//! polled `/state` fields the status bar reads, and it never projects.
//!
//! Focus model: the strip is one stop on the single vertical axis
//! (transcript → strip → composer). The focused item fills a reserved marker
//! cell with `▸` (a space when unfocused), so focus movement never shifts text
//! and stays visible in the colorless snapshot grid; the surface-color change is
//! asserted via `cell()`, not the grid.

use ratatui::{buffer::Buffer, layout::Rect};

use super::{InteractiveState, UiMode};

/// A verb the strip can carry. Dispatch happens in
/// [`activate_strip_item`](InteractiveState::activate_strip_item), always onto
/// mechanisms that also exist without the strip (pace, interrupt, gate approval).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum StripVerb {
    /// Toggle the pace hold (pause/resume).
    PauseToggle,
    /// Step the pace dial one rung slower / faster.
    Slower,
    Faster,
    /// Interrupt the in-flight tool (`POST /interrupt`).
    Kill,
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

/// One strip item: a label, the fast-path key it teaches (empty when there is
/// none), and whether it is a right-pinned universal.
#[derive(Debug, Clone, Copy)]
pub(super) struct StripItem {
    pub label: &'static str,
    pub hint: &'static str,
    pub verb: StripVerb,
    pub universal: bool,
}

const fn item(label: &'static str, hint: &'static str, verb: StripVerb) -> StripItem {
    StripItem {
        label,
        hint,
        verb,
        universal: false,
    }
}

impl InteractiveState<'_> {
    /// The ranked item list for the current state family. Re-evaluated per frame;
    /// contents only change when the underlying polled state does, so re-ranking
    /// happens at state transitions and never while the strip sits stable.
    pub(super) fn strip_items(&self) -> Vec<StripItem> {
        let held = self.pace.is_some_and(|p| p.held);
        let mut items = if self.staging.is_some() {
            // Mid-staging the composer owns the interaction (⏎ commits, ⎋
            // cancels) — the gate verbs leave the strip so `approve ⏎` can't
            // mislead or be clicked out from under the stager; the row carries
            // the staging note instead.
            vec![]
        } else if self.pending_gate.is_some() {
            vec![
                item("approve", "⏎", StripVerb::ApproveGate),
                item("allow…", "", StripVerb::AllowStage),
                item("abort", "", StripVerb::AbortGate),
            ]
        } else if held {
            vec![
                item("resume", "p", StripVerb::PauseToggle),
                item("slower", "+", StripVerb::Slower),
                item("faster", "-", StripVerb::Faster),
                item("kill", "⎋", StripVerb::Kill),
            ]
        } else if self.is_loop_active() {
            vec![
                item("pause", "⎋", StripVerb::PauseToggle),
                item("background", "b", StripVerb::Background),
                item("kill", "⎋⎋", StripVerb::Kill),
            ]
        } else {
            vec![
                item("theme", "t", StripVerb::Theme),
                item("help", "?", StripVerb::Help),
            ]
        };
        items.push(StripItem {
            label: "detach",
            hint: "q",
            verb: StripVerb::Detach,
            universal: true,
        });
        items
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
            StripVerb::Slower => self.nudge_pace_speed(true),
            StripVerb::Faster => self.nudge_pace_speed(false),
            StripVerb::Kill => self.esc_interrupt(),
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

    /// Render the strip row: contextual items from the left, universals pinned at
    /// the right edge, the focused item marked with `▸` in its reserved marker
    /// cell. A dim contextual note (redirect hint at a gate, commit/cancel hint
    /// while staging) fills the slack before the universal block. Rebuilds
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

        // Universals first, right-aligned, so the fit budget for contextuals is known.
        let universal_w: u16 = items
            .iter()
            .filter(|e| e.universal)
            .map(|e| item_width(e) + 2)
            .sum();
        let mut ux = area.width.saturating_sub(universal_w);
        let budget = ux.saturating_sub(2); // gap between the two blocks
        for (i, entry) in items.iter().enumerate() {
            if entry.universal {
                ux = self.render_strip_item(buf, area, ux, entry, i) + 2;
            }
        }

        let mut x: u16 = 1;
        for (i, entry) in items.iter().enumerate() {
            if entry.universal {
                continue;
            }
            if x + item_width(entry) > budget {
                break; // fit-to-width: lowest-ranked contextuals drop first
            }
            x = self.render_strip_item(buf, area, x, entry, i) + 2;
        }

        // The dim contextual note, truncated to the slack before the universals.
        let note = if self.staging.is_some() {
            Some("draft saved · ⏎ commit · ⎋ cancel")
        } else if self.pending_gate.is_some() {
            Some("type below to redirect")
        } else {
            None
        };
        if let Some(note) = note {
            let x = x + 1;
            if x < budget {
                let avail = (budget - x) as usize;
                let text: String = note.chars().take(avail).collect();
                put_row(buf, area, x, &text, self.theme.muted_style);
            }
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
        } else if entry.universal {
            (
                self.theme.strip_universal_style,
                self.theme.strip_universal_style,
            )
        } else {
            (self.theme.strip_item_style, self.theme.strip_hint_style)
        };
        let marker = if sel { "▸ " } else { "  " };
        let mut col = put_row(buf, area, x, marker, body);
        col = put_row(buf, area, col, entry.label, body);
        if !entry.hint.is_empty() {
            col = put_row(buf, area, col, " ", body);
            col = put_row(buf, area, col, entry.hint, hint);
        }
        col = put_row(buf, area, col, " ", body);
        self.strip_hits.push((x, col, i));
        col
    }
}

/// Display width of one rendered item (marker cell + gap, label, hint, padding).
fn item_width(entry: &StripItem) -> u16 {
    let hint_w = if entry.hint.is_empty() {
        0
    } else {
        1 + entry.hint.chars().count()
    };
    (2 + entry.label.chars().count() + hint_w + 1) as u16
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

    /// The strip is always present and never empty: idle shows the quiet set
    /// (`theme` / `help`) plus the right-pinned `detach` universal, directly above
    /// the composer.
    #[test]
    fn strip_is_always_present_and_never_empty() {
        let mut play = Screenplay::new(80, 10);
        play.user("hello");
        assert!(play.find("theme").is_some(), "idle set shows theme");
        assert!(play.find("help").is_some(), "idle set shows help");
        let (dx, _) = play.find("detach").expect("the universal is present");
        let (tx, _) = play.find("theme").unwrap();
        assert!(dx > tx, "the universal is pinned right of the contextuals");
        play.snapshot("strip_idle_set");
    }

    /// The state families swap the contextual set: a running loop shows the loop
    /// verbs, and the idle set's items are gone. (Asserted on the item list — the
    /// status bar's hint text also mentions `theme`/`help`, so `find` can't carry
    /// the negative claims.)
    #[test]
    fn strip_set_follows_the_state_family() {
        let mut play = Screenplay::new(80, 12);
        play.user("run it");
        let labels =
            |play: &Screenplay| -> Vec<&str> { play.state.strip_items().iter().map(|e| e.label).collect() };
        assert!(
            !labels(&play).contains(&"pause"),
            "no loop verbs while idle"
        );
        play.event(TuiEvent::Running(true));
        assert!(play.find("pause").is_some(), "running renders pause");
        assert!(
            labels(&play).contains(&"background"),
            "running shows background"
        );
        assert!(!labels(&play).contains(&"theme"), "the idle set is gone");
    }

    /// `↑` from the composer lands on the strip's leftmost (most relevant) item,
    /// marked with `▸` in the reserved marker cell; `→`/`←` move between items and
    /// clamp at the ends — and focus movement never shifts the labels' columns.
    #[test]
    fn strip_focus_marker_moves_without_shifting_text() {
        let mut play = Screenplay::new(80, 12);
        play.user("go");
        play.event(TuiEvent::Running(true));
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
        play.route_key(KeyCode::Up, NONE); // → strip, on `pause`
        play.route_key(KeyCode::Enter, NONE);
        assert!(play.find("HELD").is_some(), "⏎ on pause holds the loop");
        assert!(
            play.find("resume").is_some(),
            "the set flipped to the held family"
        );
        assert_eq!(
            play.state.strip_focus,
            Some(0),
            "focus stays on the landing slot (now resume)"
        );
    }

    /// The focused item's surface color differs from an unfocused item's — the
    /// glyph marker carries focus in the grid, the raised surface carries it in
    /// pixels; the grid can't show the latter, so `cell` asserts it.
    #[test]
    fn strip_focused_item_has_a_raised_surface() {
        let mut play = Screenplay::new(80, 10);
        play.user("hello");
        play.route_key(KeyCode::Up, NONE); // focus `theme`
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

    /// `⏎` on `detach` quits the UI loop (the daemon and session keep running) —
    /// the strip's right-pinned universal, reachable by keyboard.
    #[test]
    fn strip_detach_quits() {
        let mut play = Screenplay::new(80, 10);
        play.user("hello");
        play.route_key(KeyCode::Up, NONE); // idle set: theme · help · detach
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
        let labels: Vec<_> = play.state.strip_items().iter().map(|e| e.label).collect();
        assert_eq!(
            labels,
            vec!["approve", "allow…", "abort", "detach"],
            "the gate set in rank order plus the universal"
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
