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
    pub(crate) fn handle_key_event(
        &mut self,
        key: KeyEvent,
        terminal: &mut Terminal<impl Backend + Write>,
    ) -> bool {
        // Graduated Esc (docs/tui.md): a state machine, not a press-count — each Esc's effect is a
        // function of the current state (focus/overlay, held, loop-active), so no counter or timer.
        // Not-at-rest-composer → return to the composer; then held → interrupt, active → pause,
        // idle → swallow. See [`handle_escape`](InteractiveState::handle_escape).
        if key.code == KeyCode::Esc {
            self.handle_escape();
            return false;
        }

        // Ctrl+C quits from anywhere.
        if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
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
            match self.composer.handle_key(key) {
                ComposerAction::Submit(text) => self.submit_composer(text),
                ComposerAction::Consumed => {}
                ComposerAction::SpillUp => self.focus_transcript_from_composer(),
                ComposerAction::Ignored => self.handle_scroll_key(key, terminal),
            }
            return false;
        }

        self.handle_command_key(key, terminal)
    }

    /// Cross the composer's top edge into transcript navigation (docs/tui.md):
    /// leave compose mode and focus the bottom-most transcript unit (the one
    /// nearest the composer), driven by the composer's `↑`-spill at `0,0`. Entering
    /// from `VirtualBottom` and navigating up reuses ferritin's cursor/auto-scroll
    /// interaction to land on the last visible unit (or scroll if none is focusable
    /// yet). Esc returns home.
    fn focus_transcript_from_composer(&mut self) {
        use super::state::KeyboardCursor;
        self.composing = false;
        self.viewport.keyboard_cursor = KeyboardCursor::VirtualBottom;
        self.handle_navigate_up();
        self.ui.debug_message = "navigate — ↑/↓ move · ⏎ expand · esc compose".into();
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

    /// While composing, the few transcript-scroll keys the composer passes through.
    fn handle_scroll_key(&mut self, key: KeyEvent, terminal: &mut Terminal<impl Backend + Write>) {
        let page = terminal.size().map(|s| s.height / 2).unwrap_or(10);
        match key.code {
            KeyCode::PageDown => {
                self.set_scroll_offset(self.viewport.scroll_offset.saturating_add(page));
                self.sync_stuck_to_bottom();
            }
            KeyCode::PageUp => {
                self.set_scroll_offset(self.viewport.scroll_offset.saturating_sub(page));
                self.sync_stuck_to_bottom();
            }
            _ => {}
        }
    }

    /// Command-mode keys (composer unfocused): M1's single-key transcript commands.
    fn handle_command_key(
        &mut self,
        key: KeyEvent,
        terminal: &mut Terminal<impl Backend + Write>,
    ) -> bool {
        match (key.code, key.modifiers) {
            // Quit
            (KeyCode::Char('q'), _) => {
                return true;
            }

            // Move focus *down a unit* (docs/tui.md) — the y-axis of the focus model,
            // reusing ferritin's cursor + auto-scroll. `↓` past the last unit falls
            // back into the composer, the symmetric counterpart of the `↑`-spill, so
            // the transcript and composer read as one continuous vertical space. j /
            // Ctrl-n / Tab alias the arrow.
            (KeyCode::Char('j'), _)
            | (KeyCode::Down, _)
            | (KeyCode::Char('n'), KeyModifiers::CONTROL)
            | (KeyCode::Tab, _) => {
                self.handle_navigate_down();
                // Navigation owns scrolling now, so it must also re-arm/detach
                // stream-following — else the `ui_loop`'s bottom re-pin (mod.rs)
                // snaps the viewport back every frame and up-navigation can't take.
                self.sync_stuck_to_bottom();
                if matches!(
                    self.viewport.keyboard_cursor,
                    super::state::KeyboardCursor::VirtualBottom
                ) {
                    self.composing = true;
                    self.ui.debug_message = "compose — ⏎ send · ^J newline · ↑ navigate".into();
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

            // Expand the focused block
            (KeyCode::Enter, _) | (KeyCode::Char(' '), _) => {
                self.handle_activate_focused_link();
            }

            // Flip the focused tool result between its wire (distilled) and raw
            // output (docs/tui.md). No-op unless a tool-result exchange is focused.
            (KeyCode::Char('r'), _) => {
                self.toggle_raw_focused();
            }

            // Page down
            (KeyCode::Char('d'), KeyModifiers::CONTROL) | (KeyCode::PageDown, _) => {
                let page = terminal.size().map(|s| s.height / 2).unwrap_or(10);
                self.set_scroll_offset(self.viewport.scroll_offset.saturating_add(page));
                self.sync_stuck_to_bottom();
            }

            // Page up
            (KeyCode::Char('u'), KeyModifiers::CONTROL) | (KeyCode::PageUp, _) => {
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
                    self.ui.debug_message = "Mouse disabled (text selection enabled)".into();
                }
            }

            // Theme picker
            (KeyCode::Char('t'), _) => {
                let themes = RenderContext::available_themes();
                let current_theme = self
                    .current_theme_name
                    .clone()
                    .or_else(|| themes.first().cloned())
                    .unwrap_or_else(|| "default".to_string());
                let selected_index = themes.iter().position(|t| t == &current_theme).unwrap_or(0);
                self.ui_mode = UiMode::ThemePicker {
                    selected_index,
                    saved_theme_name: current_theme,
                };
                self.ui.debug_message =
                    "Select theme (↑/↓ to navigate, Enter to save, Esc to cancel)".into();
            }

            // Pace (docs/tui.md): `+`/`-` step the between-turns speed
            // dial (slower/faster), `p` toggles the pause overlay. Operator-view controls — the
            // agent never sees the pacing. `Ctrl-p` (previous unit) is matched earlier, so plain
            // `p` reaches here.
            (KeyCode::Char('+'), _) | (KeyCode::Char('='), _) => {
                self.nudge_pace_speed(true);
            }
            (KeyCode::Char('-'), _) | (KeyCode::Char('_'), _) => {
                self.nudge_pace_speed(false);
            }
            (KeyCode::Char('p'), _) => {
                self.toggle_pace_hold();
            }

            // Foreground→background handoff (docs/supervisor.md): move the
            // in-flight tool into the background supervisor instead of killing
            // it or waiting it out. Daemon-side no-op when nothing is mid-tool.
            (KeyCode::Char('b'), _) => {
                self.background_detach();
            }

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

    /// Re-arm stream-following only when the viewport is already at the bottom,
    /// so manual up-scrolls detach and a manual scroll back to the end re-attaches.
    fn sync_stuck_to_bottom(&mut self) {
        self.viewport.stuck_to_bottom = self.viewport.scroll_offset >= self.max_scroll();
    }

    /// Handle j/↓ key: navigate to next link or scroll down
    ///
    /// Implements seamless transition between link navigation and scrolling:
    /// - When no link is focused (VirtualTop): Focus first visible link, or scroll if none
    /// - When a link is focused and visible: Move to next visible link, or scroll to reveal more
    /// - When scrolling reveals new links below: Automatically focus them (auto-focus behavior)
    /// - When focused link is off-screen above (scrolled past): Re-enter by focusing first visible
    /// - When focused link is off-screen below: Scroll towards it, focusing links as they appear
    /// - When at bottom with no more links: Enter VirtualBottom state
    ///
    /// The auto-focus behavior ensures continuous j/k presses smoothly navigate through
    /// all links in the document without the user needing to manually focus each new link.
    fn handle_navigate_down(&mut self) {
        use super::state::KeyboardCursor;

        match self.viewport.keyboard_cursor {
            KeyboardCursor::VirtualTop => {
                // Try to focus first visible link
                if let Some(first_idx) = self.first_visible_link() {
                    self.viewport.keyboard_cursor = KeyboardCursor::Focused {
                        action_index: first_idx,
                    };
                } else {
                    // No visible links, just scroll down
                    let new_offset = self.viewport.scroll_offset.saturating_add(1);
                    self.set_scroll_offset(new_offset);
                }
            }
            KeyboardCursor::Focused { action_index } => {
                // Check if focused link is off-screen
                if let Some(is_above) = self.is_link_off_screen(action_index) {
                    if is_above {
                        // Focused link is above viewport - re-enter from top
                        // Moving away from focus, so immediately select first visible
                        if let Some(first_idx) = self.first_visible_link() {
                            self.viewport.keyboard_cursor = KeyboardCursor::Focused {
                                action_index: first_idx,
                            };
                        } else {
                            // No visible links - continue scrolling down
                            let old_offset = self.viewport.scroll_offset;
                            self.set_scroll_offset(old_offset.saturating_add(1));
                            if self.viewport.scroll_offset == old_offset {
                                // Couldn't scroll (at bottom) - enter virtual bottom
                                self.viewport.keyboard_cursor = KeyboardCursor::VirtualBottom;
                            }
                            // Otherwise keep current focus
                        }
                    } else {
                        // Focused link is below viewport - scroll down towards it
                        // Moving towards focus, so scroll and focus new links if they appear
                        self.set_scroll_offset(self.viewport.scroll_offset.saturating_add(1));
                        // Focus first visible link (moving towards the off-screen focus)
                        if let Some(first_idx) = self.first_visible_link() {
                            self.viewport.keyboard_cursor = KeyboardCursor::Focused {
                                action_index: first_idx,
                            };
                        }
                    }
                } else {
                    // Focused link is visible - try to move to next visible link
                    if let Some(next_idx) = self.next_visible_link(action_index) {
                        self.viewport.keyboard_cursor = KeyboardCursor::Focused {
                            action_index: next_idx,
                        };
                    } else {
                        // No more visible links below - scroll down and watch for new links
                        let old_offset = self.viewport.scroll_offset;
                        self.set_scroll_offset(old_offset.saturating_add(1));

                        // If we actually scrolled, check if a new link appeared
                        if self.viewport.scroll_offset > old_offset {
                            // Check if there's now a next visible link
                            if let Some(next_idx) = self.next_visible_link(action_index) {
                                // Auto-focus the newly visible link
                                self.viewport.keyboard_cursor = KeyboardCursor::Focused {
                                    action_index: next_idx,
                                };
                            }
                            // Otherwise keep current focus (will scroll off-screen eventually)
                        } else {
                            // Couldn't scroll (at bottom) - enter virtual bottom state
                            self.viewport.keyboard_cursor = KeyboardCursor::VirtualBottom;
                        }
                    }
                }
            }
            KeyboardCursor::VirtualBottom => {
                // Can't go further down
            }
        }
    }

    /// Handle k/↑ key: navigate to previous link or scroll up
    ///
    /// Mirror of handle_navigate_down, moving upward through the document:
    /// - When no link is focused (VirtualTop): Do nothing (can't go above virtual top)
    /// - When a link is focused and visible: Move to previous visible link, or scroll to reveal more
    /// - When scrolling reveals new links above: Automatically focus them (auto-focus behavior)
    /// - When focused link is off-screen below (scrolled past): Re-enter by focusing last visible
    /// - When focused link is off-screen above: Scroll towards it, focusing links as they appear
    /// - When at top with no more links: Enter VirtualTop state
    /// - From VirtualBottom: Focus last visible link, or scroll if none
    fn handle_navigate_up(&mut self) {
        use super::state::KeyboardCursor;

        match self.viewport.keyboard_cursor {
            KeyboardCursor::VirtualTop => {
                // Can't go higher than virtual top
            }
            KeyboardCursor::Focused { action_index } => {
                // Check if focused link is off-screen
                if let Some(is_above) = self.is_link_off_screen(action_index) {
                    if is_above {
                        // Focused link is above viewport - scroll up towards it
                        // Moving towards focus, so scroll and focus new links if they appear
                        self.set_scroll_offset(self.viewport.scroll_offset.saturating_sub(1));
                        // Focus last visible link (moving towards the off-screen focus)
                        if let Some(last_idx) = self.last_visible_link() {
                            self.viewport.keyboard_cursor = KeyboardCursor::Focused {
                                action_index: last_idx,
                            };
                        }
                    } else {
                        // Focused link is below viewport - re-enter from bottom
                        // Moving away from focus, so immediately select last visible
                        if let Some(last_idx) = self.last_visible_link() {
                            self.viewport.keyboard_cursor = KeyboardCursor::Focused {
                                action_index: last_idx,
                            };
                        } else {
                            // No visible links - continue scrolling up
                            let old_offset = self.viewport.scroll_offset;
                            self.set_scroll_offset(old_offset.saturating_sub(1));
                            if self.viewport.scroll_offset == old_offset {
                                // Couldn't scroll (at top) - enter virtual top
                                self.viewport.keyboard_cursor = KeyboardCursor::VirtualTop;
                            }
                            // Otherwise keep current focus
                        }
                    }
                } else {
                    // Focused link is visible - try to move to previous visible link
                    if let Some(prev_idx) = self.prev_visible_link(action_index) {
                        self.viewport.keyboard_cursor = KeyboardCursor::Focused {
                            action_index: prev_idx,
                        };
                    } else {
                        // No more visible links above - scroll up and watch for new links
                        let old_offset = self.viewport.scroll_offset;
                        self.set_scroll_offset(old_offset.saturating_sub(1));

                        // If we actually scrolled, check if a new link appeared
                        if self.viewport.scroll_offset < old_offset {
                            // Check if there's now a prev visible link
                            if let Some(prev_idx) = self.prev_visible_link(action_index) {
                                // Auto-focus the newly visible link
                                self.viewport.keyboard_cursor = KeyboardCursor::Focused {
                                    action_index: prev_idx,
                                };
                            }
                            // Otherwise keep current focus (will scroll off-screen eventually)
                        } else {
                            // Couldn't scroll (at top) - enter virtual top state
                            self.viewport.keyboard_cursor = KeyboardCursor::VirtualTop;
                        }
                    }
                }
            }
            KeyboardCursor::VirtualBottom => {
                // Re-enter from bottom - focus last visible link
                if let Some(last_idx) = self.last_visible_link() {
                    self.viewport.keyboard_cursor = KeyboardCursor::Focused {
                        action_index: last_idx,
                    };
                } else {
                    // No visible links, just scroll up
                    let new_offset = self.viewport.scroll_offset.saturating_sub(1);
                    self.set_scroll_offset(new_offset);
                }
            }
        }
    }

    /// Handle Enter/Space: activate the focused link
    ///
    /// Activates the currently focused link (if any), triggering the same action
    /// as clicking it with the mouse. Navigation actions send a command to the
    /// request thread and reset keyboard focus to VirtualTop for the new document.
    /// ExpandBlock actions mutate the document in place and preserve focus.
    /// Does nothing when in VirtualTop or VirtualBottom states.
    fn handle_activate_focused_link(&mut self) {
        use super::state::KeyboardCursor;

        if let KeyboardCursor::Focused { action_index } = self.viewport.keyboard_cursor
            && let Some((_, action)) = self.render_cache.actions.get(action_index)
        {
            let action = action.clone();

            // Handle SelectTheme specially (same as mouse click)
            if let crate::tui::document::TuiAction::SelectTheme(theme_name) = &action {
                let _ = self.apply_theme(theme_name);
                if let super::UiMode::ThemePicker {
                    ref mut selected_index,
                    ..
                } = self.ui_mode
                {
                    let themes = crate::tui::render_context::RenderContext::available_themes();
                    if let Some(idx) = themes.iter().position(|t| t == theme_name.as_ref()) {
                        *selected_index = idx;
                    }
                }
                self.ui.debug_message = format!("Selected theme: {theme_name}").into();
            } else {
                // ExpandBlock mutates the document in place; invalidate the
                // layout cache so the next frame re-measures.
                super::events::handle_action(&mut self.document.document, &mut self.view, action);
                self.viewport.cached_layout = None;
            }
        }
    }
}
