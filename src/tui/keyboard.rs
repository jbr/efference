use std::io::Write;

use crossterm::{
    event::{DisableMouseCapture, EnableMouseCapture, KeyCode, KeyEvent, KeyModifiers},
    execute,
};
use ratatui::{Terminal, prelude::Backend};

use super::{InteractiveState, UiMode};
use crate::tui::composer::ComposerAction;
use crate::tui::render_context::RenderContext;

impl<'a> InteractiveState<'a> {
    pub(super) fn handle_composer_key(&mut self, key: KeyEvent) -> ComposerAction {
        let super_key = key.modifiers.contains(KeyModifiers::SUPER);
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        let char_with = |ch| matches!(key.code, KeyCode::Char(actual) if actual == ch);
        if char_with('w') && alt && !super_key {
            self.copy_selection();
            ComposerAction::Consumed
        } else if char_with('w') && ctrl {
            self.cut_selection_or_delete_word(key);
            ComposerAction::Consumed
        } else if char_with('r') && ctrl && !alt && !super_key {
            // Recall the most recent queued-but-unsent turn into the composer (docs/tui.md).
            self.recall_pending_turn();
            ComposerAction::Consumed
        } else if char_with('y') && ctrl && !alt {
            self.paste_clipboard();
            ComposerAction::Consumed
        } else if char_with('c') && super_key && !alt {
            self.copy_selection();
            ComposerAction::Consumed
        } else if char_with('x') && super_key && !alt {
            self.cut_selection_or_delete_word(key);
            ComposerAction::Consumed
        } else if char_with('v') && super_key && !alt {
            self.paste_clipboard();
            ComposerAction::Consumed
        } else if char_with('z') && super_key && key.modifiers.contains(KeyModifiers::SHIFT) {
            self.composer.redo();
            ComposerAction::Consumed
        } else if char_with('z') && super_key && !alt {
            self.composer.undo();
            ComposerAction::Consumed
        } else {
            self.composer.handle_key(key)
        }
    }

    pub(crate) fn handle_key_event(
        &mut self,
        key: KeyEvent,
        terminal: &mut Terminal<impl Backend + Write>,
    ) -> bool {
        // The nested form owns Esc and Ctrl-C: neither reaches the attached loop.
        #[cfg(feature = "dev")]
        if let UiMode::Sessions(menu) = &mut self.ui_mode
            && let Some(form) = &mut menu.form
        {
            use super::session_form::Action;
            match form.handle(key) {
                Action::Continue => {}
                Action::Cancel => menu.form = None,
                Action::Start(_) => form.creation_failed(
                    "Demo creation error · no request sent; edits kept. Esc returns to Sessions."
                        .into(),
                ),
            }
            return false;
        }
        // Graduated Esc (docs/tui.md): a state machine, not a press-count, so it is handled
        // before any mode dispatch. See [`handle_escape`](InteractiveState::handle_escape).
        if key.code == KeyCode::Esc {
            self.handle_escape();
            return false;
        }

        // Ctrl+C quits from anywhere.
        if key.code == KeyCode::Char('c')
            && key.modifiers.contains(KeyModifiers::CONTROL)
            && !key.modifiers.contains(KeyModifiers::SUPER)
        {
            return true;
        }

        // Dev-only (gated by EFFERENCE_DEV): Ctrl+S dumps the current screen to a
        // timestamped framed grid under `.efference/screenshots/` — the live cousin
        // of a committed snapshot, so reachable states can be eyeballed/shown without
        // a recording (docs/tui.md). Works from any mode.
        if key.code == KeyCode::Char('s')
            && std::env::var_os("EFFERENCE_DEV").is_some()
            && (key.modifiers.contains(KeyModifiers::CONTROL)
                || key.modifiers.contains(KeyModifiers::ALT))
        {
            // Ctrl+S = full screen; Alt+S = the sidebar columns only (a narrow capture).
            let sidebar_only = key.modifiers.contains(KeyModifiers::ALT);
            self.dev_screenshot(terminal, sidebar_only);
            return false;
        }

        if matches!(self.ui_mode, UiMode::Help) {
            // Any key (except Escape, handled above) closes help.
            self.ui_mode = UiMode::Normal;
            return false;
        }

        if matches!(self.ui_mode, UiMode::Sessions(_)) {
            return self.handle_sessions_key(key);
        }

        if let UiMode::ThemePicker {
            ref mut selected_index,
            ..
        } = self.ui_mode
        {
            let themes = RenderContext::available_themes();
            let theme_count = themes.len();
            match key.code {
                KeyCode::Up | KeyCode::Char('k') if *selected_index > 0 => {
                    *selected_index -= 1;
                    if let Some(theme_name) = themes.get(*selected_index) {
                        let _ = self.apply_theme(theme_name);
                    }
                }
                KeyCode::Down | KeyCode::Char('j') if *selected_index + 1 < theme_count => {
                    *selected_index += 1;
                    if let Some(theme_name) = themes.get(*selected_index) {
                        let _ = self.apply_theme(theme_name);
                    }
                }
                KeyCode::Enter => {
                    let theme_name = self
                        .current_theme_name
                        .clone()
                        .unwrap_or_else(|| "default".into());
                    self.ui_mode = UiMode::Normal;
                    self.ui.debug_message = format!("Theme saved: {theme_name}").into();
                }
                _ => {}
            }
            return false;
        }

        // Compose mode: keys go to the composer. A key the composer doesn't want
        // (PgUp/PgDn) still scrolls the transcript so you can read while typing.
        if self.composing {
            // C-d on an empty composer quits, shell/REPL EOF style (the composer
            // otherwise binds C-d to delete-forward, so the empty-buffer guard is
            // what disambiguates "delete the char ahead" from "nothing left — exit").
            if key.code == KeyCode::Char('d')
                && key.modifiers.contains(KeyModifiers::CONTROL)
                && self.composer.is_empty()
            {
                return true;
            }

            // A staged verb's ⏎ commits the argument (even empty — bare `.allow`
            // derives its grant), ahead of the gate's own Enter handling.
            if self.staging.is_some()
                && key.code == KeyCode::Enter
                && !key.modifiers.contains(KeyModifiers::ALT)
            {
                self.commit_staging();
                return false;
            }

            // While a gate is pending, Enter always resolves it — even on an empty
            // buffer (= approve), which the composer would otherwise swallow.
            if self.pending_gate.is_some()
                && key.code == KeyCode::Enter
                && !key.modifiers.contains(KeyModifiers::ALT)
            {
                let text = self.composer.text();
                self.submit_composer(text);
                return false;
            }
            // Empty composer, operator's turn: `⏎` hands control back with no input
            // (docs/tui.md) — the zero-token continue, which the composer would
            // otherwise swallow as a no-op. Gated on the loop being idle, so a stray
            // `⏎` during a generation stays the silent no-op it has always been; the
            // daemon re-checks the same preconditions and declines a race.
            if key.code == KeyCode::Enter
                && !key.modifiers.contains(KeyModifiers::ALT)
                && self.composer.is_empty()
                && !self.read_only()
                && !self.is_loop_active()
            {
                self.continue_trajectory();
                return false;
            }
            match self.handle_composer_key(key) {
                ComposerAction::Submit(text) => self.submit_composer(text),
                ComposerAction::Consumed => {}
                // The `↑`-spill at the 0,0 detent lands on the control strip —
                // the middle stop of the transcript → strip → composer axis
                // (docs/tui.md). `↑` again from the strip enters the transcript.
                ComposerAction::SpillUp => self.focus_strip_from_composer(),
                ComposerAction::Ignored => self.handle_scroll_key(key, terminal),
            }
            return false;
        }

        if let Some(verb) = self.strip_focus {
            return self.handle_strip_key(key, verb, terminal);
        }
        if self.sidebar_open {
            return self.handle_sidebar_key(key, terminal);
        }

        self.handle_command_key(key, terminal)
    }

    /// Keys while the control strip is focused (docs/tui.md): `←`/`→` (and
    /// `C-b`/`C-f`) move between items, `⏎`/space activates, `↑`/`k` continues
    /// into the transcript, `↓`/`j` drops back to the composer. A plain letter
    /// matching a *shown* item's hint is that item's **scoped mnemonic**, so the
    /// pace, hold, background, and theme verbs answer to their letters only where
    /// their item is on the strip (e.g. `p` resumes only in the held set), never as
    /// buried globals.
    /// Anything unmatched falls through to the remaining transcript keys (scroll,
    /// help, quit).
    fn handle_strip_key(
        &mut self,
        key: KeyEvent,
        verb: super::control_strip::StripVerb,
        terminal: &mut Terminal<impl Backend + Write>,
    ) -> bool {
        // An active pace widget owns the row (docs/tui.md): only it captures `↑`/`↓`
        // (step the dial), `⏎`/space commits; everything else is a captured no-op so
        // the stepper can't be navigated out of mid-adjust (Esc reverts, upstream in
        // `handle_escape`). `↑` climbs toward more delay, matching the `▴` on top.
        if self.pace_widget.is_some() {
            match key.code {
                KeyCode::Up => self.step_pace_widget(true),
                KeyCode::Down => self.step_pace_widget(false),
                KeyCode::Enter | KeyCode::Char(' ') => self.commit_pace_widget(),
                _ => {}
            }
            return false;
        }

        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Up | KeyCode::BackTab => {
                self.strip_focus = None;
                self.focus_transcript_from_composer();
                false
            }
            KeyCode::Char('k') if !ctrl => {
                self.strip_focus = None;
                self.focus_transcript_from_composer();
                false
            }
            KeyCode::Char('p') if ctrl => {
                self.strip_focus = None;
                self.focus_transcript_from_composer();
                false
            }
            // Tab toggles the sidebar (docs/tui.md) from the strip as from the
            // transcript; `↓` alone still drops to the composer.
            KeyCode::Tab => {
                self.toggle_sidebar();
                false
            }
            KeyCode::Down => {
                self.leave_strip_down();
                false
            }
            KeyCode::Char('j') if !ctrl => {
                self.leave_strip_down();
                false
            }
            KeyCode::Char('n') if ctrl => {
                self.leave_strip_down();
                false
            }
            KeyCode::Left => {
                self.move_strip_focus(false);
                false
            }
            KeyCode::Char('b') if ctrl => {
                self.move_strip_focus(false);
                false
            }
            KeyCode::Right => {
                self.move_strip_focus(true);
                false
            }
            KeyCode::Char('f') if ctrl => {
                self.move_strip_focus(true);
                false
            }
            KeyCode::Enter | KeyCode::Char(' ') => {
                self.activate_strip_verb(verb);
                self.ui.quit_requested
            }
            // Scoped mnemonic: a plain letter that names a shown item fires it,
            // wherever focus currently sits (a shortcut, not a focus move). Items
            // whose hint isn't a bare letter (Esc's `⎋`, `allow…`'s empty hint) are
            // driven by their own key or the arrows, never matched here.
            KeyCode::Char(c) if !ctrl => {
                match self
                    .strip_items()
                    .iter()
                    .find(|it| it.hint == c.to_string())
                    .map(|it| it.verb)
                {
                    Some(verb) => {
                        self.activate_strip_verb(verb);
                        self.ui.quit_requested
                    }
                    None => self.handle_command_key(key, terminal),
                }
            }
            _ => self.handle_command_key(key, terminal),
        }
    }

    /// `↓` off the bottom of the strip — the last stop on the way down, which is
    /// the composer. In a read-only view (docs/tui.md) there is no composer below,
    /// so the strip keeps focus and the key is a no-op.
    fn leave_strip_down(&mut self) {
        if self.read_only() {
            return;
        }
        self.strip_focus = None;
        self.composing = true;
        self.ui.debug_message = "compose — ⏎ send · ^J newline · ↑ navigate".into();
    }

    /// Capture the live screen to a timestamped framed-grid file under
    /// `.efference/screenshots/` and report the path in the status bar. The dev
    /// screenshot keybind (`Ctrl+S`, gated by `EFFERENCE_DEV`); see
    /// [`capture_screenshot_grid`](InteractiveState::capture_screenshot_grid).
    fn dev_screenshot(
        &mut self,
        terminal: &mut Terminal<impl Backend + Write>,
        sidebar_only: bool,
    ) {
        let (w, h) = terminal
            .size()
            .map(|s| (s.width, s.height))
            .unwrap_or((80, 24));
        let grid = if sidebar_only {
            self.capture_sidebar_grid(w, h)
        } else {
            self.capture_screenshot_grid(w, h)
        };
        let millis = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0);
        let dir = std::path::Path::new(".efference/screenshots");
        let kind = if sidebar_only { "sidebar" } else { "tui" };
        let path = dir.join(format!("{kind}-{millis}.txt"));
        self.ui.debug_message =
            match std::fs::create_dir_all(dir).and_then(|()| std::fs::write(&path, &grid)) {
                Ok(()) => format!("screenshot → {}", path.display()).into(),
                Err(e) => format!("screenshot failed: {e}").into(),
            };
    }

    /// Command-mode keys (composer unfocused): the single-key transcript commands.
    /// `pub(super)`: the sidebar falls unmatched keys through to it, as the strip does.
    pub(super) fn handle_command_key(
        &mut self,
        key: KeyEvent,
        terminal: &mut Terminal<impl Backend + Write>,
    ) -> bool {
        match (key.code, key.modifiers) {
            // Quit
            (KeyCode::Char('q'), _) => {
                return true;
            }

            // Tab toggles the sidebar (docs/tui.md) — the second column focus can
            // move onto from command mode. It is not an alias for `↓`, which (with
            // j / C-n) keeps down-a-unit to itself.
            (KeyCode::Tab, _) => {
                self.toggle_sidebar();
            }

            // Move focus *down a unit* (docs/tui.md) — the y-axis of the focus model,
            // reusing ferritin's cursor + auto-scroll. `↓` past the last unit falls
            // back into the composer, the symmetric counterpart of the `↑`-spill, so
            // the transcript and composer read as one continuous vertical space. j /
            // Ctrl-n alias the arrow.
            (KeyCode::Char('j'), _)
            | (KeyCode::Down, _)
            | (KeyCode::Char('n'), KeyModifiers::CONTROL) => {
                self.handle_navigate_down();
                // Navigation owns scrolling now, so it must also re-arm/detach
                // stream-following — else the `ui_loop`'s bottom re-pin (mod.rs)
                // snaps the viewport back every frame and up-navigation can't take.
                self.sync_stuck_to_bottom();
                if matches!(
                    self.viewport.keyboard_cursor,
                    super::state::KeyboardCursor::VirtualBottom
                ) {
                    // `↓` past the last unit stops at the control strip — the
                    // symmetric counterpart of the composer's `↑`-spill
                    // (docs/tui.md); `↓` again reaches the composer.
                    self.focus_strip_from_transcript();
                }
            }

            // Move focus *up a unit*. k / Ctrl-p / BackTab alias the arrow.
            (KeyCode::Char('k'), _)
            | (KeyCode::Up, _)
            | (KeyCode::Char('p'), KeyModifiers::CONTROL)
            | (KeyCode::BackTab, _) => {
                self.handle_navigate_up();
                self.sync_stuck_to_bottom();
            }

            // The block menu (docs/tui.md): `←`/`→` (and `C-b`/`C-f`) choose among
            // the focused unit's entry verbs. The Ctrl arms are matched here, ahead
            // of the plain `b` background bind below.
            (KeyCode::Left, _) | (KeyCode::Char('b'), KeyModifiers::CONTROL) => {
                self.move_block_menu(false);
            }
            (KeyCode::Right, _) | (KeyCode::Char('f'), KeyModifiers::CONTROL) => {
                self.move_block_menu(true);
            }

            // Fire the focused unit's selected verb — the collapse toggle unless the
            // menu selection has moved, so `⏎` keeps meaning "expand this."
            (KeyCode::Enter, _) | (KeyCode::Char(' '), _) => {
                self.activate_block_menu();
            }

            // Page down
            (KeyCode::Char('d'), KeyModifiers::CONTROL)
            | (KeyCode::Char('v'), KeyModifiers::CONTROL)
            | (KeyCode::PageDown, _) => {
                let page = terminal.size().map(|s| s.height / 2).unwrap_or(10);
                self.set_scroll_offset(self.viewport.scroll_offset.saturating_add(page));
                self.sync_stuck_to_bottom();
            }

            // Page up
            (KeyCode::Char('u'), KeyModifiers::CONTROL)
            | (KeyCode::Char('v'), KeyModifiers::ALT)
            | (KeyCode::PageUp, _) => {
                let page = terminal.size().map(|s| s.height / 2).unwrap_or(10);
                self.set_scroll_offset(self.viewport.scroll_offset.saturating_sub(page));
                self.sync_stuck_to_bottom();
            }

            // Jump to top
            (KeyCode::Char('g'), _) | (KeyCode::Home, _) => {
                self.set_scroll_offset(0);
                self.viewport.stuck_to_bottom = false;
            }

            // Jump to bottom (and re-arm stream-following)
            (KeyCode::Char('G'), _) | (KeyCode::End, _) => {
                self.set_scroll_offset(u16::MAX); // clamps to max
                self.viewport.stuck_to_bottom = true;
            }

            // Toggle mouse mode for text selection
            (KeyCode::Char('m'), _) => {
                self.ui.mouse_enabled = !self.ui.mouse_enabled;
                if self.ui.mouse_enabled {
                    let _ = execute!(terminal.backend_mut(), EnableMouseCapture);
                    self.ui.debug_message = "Mouse enabled (hover/click)".into();
                } else {
                    let _ = execute!(terminal.backend_mut(), DisableMouseCapture);
                    self.viewport.cursor_pos = None;
                    // No further `Moved` events will arrive to clear it, and a hover
                    // that cannot be left would pin the strip for good.
                    self.viewport.strip_hovered = false;
                    self.ui.debug_message = "Mouse disabled (text selection enabled)".into();
                }
            }

            // Bind purge (docs/tui.md): the transcript-navigation
            // single-letter verbs — `t` theme, `p` pause, `+`/`-` pace, `b`
            // background, `r` raw — are gone. Each now lives where it is shown:
            // pace/hold/background/theme on the control strip (reached with `↑`,
            // fired by a click or the item's scoped-mnemonic key while the strip is
            // focused — see `handle_strip_key`), and the raw flip on the block menu
            // (`←`/`→` on a focused exchange). A plain letter in the transcript now
            // only ever no-ops (the `_` arm): it never types, and never fires a
            // buried verb. `Ctrl-b`/`Ctrl-f` (block-menu move) and `Ctrl-p`/`Ctrl-n`
            // (unit nav) are matched above, so the plain letters can fall through.

            // Help
            (KeyCode::Char('?'), _) | (KeyCode::Char('h'), _) => {
                self.ui_mode = UiMode::Help;
            }

            // Clean contextual modes (docs/tui.md): the transcript owns its keymap, so
            // an unbound key is a no-op here — typing does *not* leak into the composer.
            // Return to the composer deliberately: Esc, `↓` off the bottom, or the
            // composer's own `↑`-spill in reverse.
            _ => {}
        }
        false
    }
}

mod navigate;
