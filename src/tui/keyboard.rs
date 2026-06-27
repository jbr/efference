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
        // Escape closes help, reverts the theme picker, or — in Normal mode —
        // toggles between composing (keys go to the composer) and command mode
        // (M1's single-key transcript commands).
        if key.code == KeyCode::Esc {
            match std::mem::replace(&mut self.ui_mode, UiMode::Normal) {
                UiMode::Help => {}
                UiMode::ThemePicker {
                    saved_theme_name, ..
                } => {
                    let _ = self.apply_theme(&saved_theme_name);
                }
                UiMode::Normal => {
                    self.composing = !self.composing;
                    self.ui.debug_message = if self.composing {
                        "compose — ⏎ send · ^J newline · esc command".into()
                    } else {
                        "command — i compose · j/k scroll · t theme · ? help · q quit".into()
                    };
                }
            }
            return false;
        }

        // Ctrl+C quits from anywhere.
        if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
            return true;
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
                ComposerAction::Ignored => self.handle_scroll_key(key, terminal),
            }
            return false;
        }

        self.handle_command_key(key, terminal)
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

            // Line scroll down
            (KeyCode::Char('j'), _)
            | (KeyCode::Down, _)
            | (KeyCode::Char('n'), KeyModifiers::CONTROL) => {
                self.set_scroll_offset(self.viewport.scroll_offset.saturating_add(1));
                self.sync_stuck_to_bottom();
            }

            // Line scroll up
            (KeyCode::Char('k'), _)
            | (KeyCode::Up, _)
            | (KeyCode::Char('p'), KeyModifiers::CONTROL) => {
                self.set_scroll_offset(self.viewport.scroll_offset.saturating_sub(1));
                self.sync_stuck_to_bottom();
            }

            // Move focus between collapsible blocks (the only "links" in a
            // transcript). Reuses ferritin's link cursor + auto-scroll.
            (KeyCode::Tab, _) => self.handle_navigate_down(),
            (KeyCode::BackTab, _) => self.handle_navigate_up(),

            // Expand the focused block
            (KeyCode::Enter, _) | (KeyCode::Char(' '), _) => {
                self.handle_activate_focused_link();
            }

            // Flip the focused tool result between its wire (distilled) and raw
            // output (DESIGN §10.5). No-op unless a tool-result exchange is focused.
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

            // Help
            (KeyCode::Char('?'), _) | (KeyCode::Char('h'), _) => {
                self.ui_mode = UiMode::Help;
            }

            // Return to compose mode.
            (KeyCode::Char('i'), _) | (KeyCode::Char('a'), _) => {
                self.composing = true;
                self.ui.debug_message = "compose — ⏎ send · ^J newline · esc command".into();
            }

            // Any other printable key re-enters compose and types itself.
            (KeyCode::Char(c), m)
                if !m.contains(KeyModifiers::CONTROL) && !m.contains(KeyModifiers::ALT) =>
            {
                self.composing = true;
                let _ = self.composer.handle_key(KeyEvent::new(KeyCode::Char(c), m));
            }

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
