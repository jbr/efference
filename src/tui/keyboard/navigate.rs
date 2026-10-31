//! Transcript navigation: the keyboard cursor, link activation, and scroll sync (docs/tui.md).

use super::*;

impl<'a> InteractiveState<'a> {
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
    pub(super) fn handle_navigate_down(&mut self) {
        use crate::tui::state::KeyboardCursor;

        // The block menu travels with focus: a new unit starts on its leading verb,
        // so `⏎` after a move is always "the most relevant thing" (docs/tui.md).
        self.block_menu_focus = 0;

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
    pub(super) fn handle_navigate_up(&mut self) {
        use crate::tui::state::KeyboardCursor;

        // As in `handle_navigate_down`: the menu selection belongs to the unit.
        self.block_menu_focus = 0;

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
    pub(in crate::tui) fn handle_activate_focused_link(&mut self) {
        use crate::tui::state::KeyboardCursor;

        if let KeyboardCursor::Focused { action_index } = self.viewport.keyboard_cursor
            && let Some((_, action)) = self.render_cache.actions.get(action_index)
        {
            let action = action.clone();

            // A folded item opens whole (docs/tui.md, "Detail by recency").
            if let crate::tui::document::TuiAction::ExpandItem(key) = &action {
                self.toggle_expanded(key.to_string());
                return;
            }
            // Handle SelectTheme specially (same as mouse click)
            if let crate::tui::document::TuiAction::SelectTheme(theme_name) = &action {
                let _ = self.apply_theme(theme_name);
                if let UiMode::ThemePicker {
                    ref mut selected_index,
                    ..
                } = self.ui_mode
                {
                    let themes = RenderContext::available_themes();
                    if let Some(idx) = themes.iter().position(|t| t == theme_name.as_ref()) {
                        *selected_index = idx;
                    }
                }
                self.ui.debug_message = format!("Selected theme: {theme_name}").into();
            } else {
                // ExpandBlock mutates the document in place; `handle_action`
                // drops the affected node's cached layout.
                crate::tui::events::handle_action(&mut self.document, &mut self.view, action);
            }
        }
    }

    /// Re-arm stream-following only when the viewport is already at the bottom,
    /// so manual up-scrolls detach and a manual scroll back to the end re-attaches.
    pub(super) fn sync_stuck_to_bottom(&mut self) {
        self.viewport.stuck_to_bottom = self.viewport.scroll_offset >= self.max_scroll();
    }

    /// Cross the composer's top edge into transcript navigation (docs/tui.md):
    /// leave compose mode and focus the bottom-most transcript unit (the one
    /// nearest the composer), driven by the composer's `↑`-spill at `0,0`. Entering
    /// from `VirtualBottom` and navigating up reuses ferritin's cursor/auto-scroll
    /// interaction to land on the last visible unit (or scroll if none is focusable
    /// yet). Esc returns home.
    pub(super) fn focus_transcript_from_composer(&mut self) {
        use crate::tui::state::KeyboardCursor;
        self.composing = false;
        self.viewport.keyboard_cursor = KeyboardCursor::VirtualBottom;
        self.handle_navigate_up();
        // The home Esc returns to differs in a read-only view (no composer, so the
        // strip is the steady state) — the hint names whichever one applies.
        self.ui.debug_message = if self.read_only() {
            "navigate — ↑/↓ unit · ←/→ verb · ⏎ act · esc controls".into()
        } else {
            "navigate — ↑/↓ unit · ←/→ verb · ⏎ act · esc compose".into()
        };
    }

    /// While composing, the few transcript-scroll keys the composer passes through.
    pub(super) fn handle_scroll_key(
        &mut self,
        key: KeyEvent,
        terminal: &mut Terminal<impl Backend + Write>,
    ) {
        let page = terminal.size().map_or(10, |s| s.height / 2);
        match (key.code, key.modifiers) {
            (KeyCode::PageDown, _) | (KeyCode::Char('v'), KeyModifiers::CONTROL) => {
                self.set_scroll_offset(self.viewport.scroll_offset.saturating_add(page));
                self.sync_stuck_to_bottom();
            }
            (KeyCode::PageUp, _) | (KeyCode::Char('v'), KeyModifiers::ALT) => {
                self.set_scroll_offset(self.viewport.scroll_offset.saturating_sub(page));
                self.sync_stuck_to_bottom();
            }
            _ => {}
        }
    }
}
