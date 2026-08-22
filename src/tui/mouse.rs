use std::io::Write;

use crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::{Terminal, layout::Position, prelude::Backend};

use super::SidebarReturn;
use super::events::handle_action;
use super::utils::set_cursor_shape;
use crate::tui::document::TuiAction;
use crate::tui::render_context::RenderContext;

use super::UiMode;

impl<'a> super::InteractiveState<'a> {
    pub(super) fn handle_mouse_event(
        &mut self,
        mouse_event: MouseEvent,
        terminal: &Terminal<impl Backend>,
    ) {
        if !self.ui.mouse_enabled {
            return;
        }

        match mouse_event {
            MouseEvent {
                kind: MouseEventKind::Moved,
                column,
                row,
                ..
            } => {
                let Ok(size) = terminal.size() else {
                    return;
                };

                // In ThemePicker mode, use absolute screen coordinates (no scroll offset)
                if matches!(self.ui_mode, super::UiMode::ThemePicker { .. }) {
                    self.viewport.cursor_pos = Some(Position::new(column, row));
                    return;
                }

                let terminal_height = size.height;
                let terminal_width = size.width;
                let content_height = terminal_height
                    .saturating_sub(1 + self.viewport.input_height + self.viewport.gate_height); // status row + composer
                let content_width = terminal_width.saturating_sub(1); // Exclude scrollbar column

                // Check if hovering over scrollbar
                self.viewport.scrollbar_hovered =
                    row < content_height && column == content_width && self.scrollbar_visible();

                if row < content_height {
                    // Mouse in main content area
                    self.viewport.cursor_pos =
                        Some(Position::new(column, row + self.viewport.scroll_offset));
                } else {
                    // Mouse over the breadcrumb/status rows (no hover targets there)
                    self.viewport.cursor_pos = None;
                }
            }

            MouseEvent {
                kind: MouseEventKind::ScrollDown,
                ..
            } => {
                self.set_scroll_offset(self.viewport.scroll_offset.saturating_add(1));
                self.viewport.stuck_to_bottom = self.viewport.scroll_offset >= self.max_scroll();
            }

            MouseEvent {
                kind: MouseEventKind::ScrollUp,
                ..
            } => {
                self.set_scroll_offset(self.viewport.scroll_offset.saturating_sub(1));
                self.viewport.stuck_to_bottom = false;
            }

            MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column,
                row,
                modifiers,
            } => {
                if let Some(area) = self.viewport.composer_area
                    && area.contains(Position::new(column, row))
                {
                    let cursor = self.composer.cursor_for_screen_position(area, column, row);
                    self.composer
                        .set_cursor_from_mouse(cursor, modifiers.contains(KeyModifiers::SHIFT));
                    self.viewport.composer_dragging = true;
                    self.composing = true;
                    self.strip_focus = None;
                    return;
                }

                let Ok(size) = terminal.size() else {
                    return;
                };

                let terminal_height = size.height;
                let content_height = terminal_height
                    .saturating_sub(1 + self.viewport.input_height + self.viewport.gate_height); // status row + composer
                let content_width = size.width.saturating_sub(1); // Exclude scrollbar column

                // The control strip row sits directly above the composer
                // (docs/tui.md); a click on an item focuses and activates it.
                let strip_row = terminal_height.saturating_sub(2 + self.viewport.input_height);

                // Check if click is in scrollbar column
                if row < content_height && column == content_width && self.scrollbar_visible() {
                    // Start scrollbar drag
                    self.viewport.scrollbar_dragging = true;
                    // Calculate scroll position from click Y
                    self.handle_scrollbar_drag(row, content_height);
                } else if row < content_height {
                    // Click in main content area
                    self.viewport.clicked_position =
                        Some(Position::new(column, row + self.viewport.scroll_offset));
                } else if row == strip_row
                    && let Some(idx) = self
                        .strip_hits
                        .iter()
                        .find(|(x0, x1, _)| column >= *x0 && column < *x1)
                        .map(|(_, _, i)| *i)
                {
                    self.composing = false;
                    self.strip_focus = Some(idx);
                    self.activate_strip_item(idx);
                }
            }

            MouseEvent {
                kind: MouseEventKind::Drag(MouseButton::Left),
                column,
                row,
                ..
            } if self.viewport.composer_dragging => {
                if let Some(area) = self.viewport.composer_area {
                    let cursor = self.composer.cursor_for_screen_position(area, column, row);
                    self.composer.extend_selection_from_mouse(cursor);
                }
            }

            MouseEvent {
                kind: MouseEventKind::Up(MouseButton::Left),
                ..
            } if self.viewport.composer_dragging => {
                self.viewport.composer_dragging = false;
            }

            MouseEvent {
                kind: MouseEventKind::Drag(_),
                row,
                ..
            } if self.viewport.scrollbar_dragging => {
                let Ok(size) = terminal.size() else {
                    return;
                };
                let content_height = size
                    .height
                    .saturating_sub(1 + self.viewport.input_height + self.viewport.gate_height);
                self.handle_scrollbar_drag(row, content_height);
            }

            MouseEvent {
                kind: MouseEventKind::Up(_),
                ..
            } if self.viewport.scrollbar_dragging => {
                self.viewport.scrollbar_dragging = false;
            }
            _ => { /*unhandled*/ }
        }
    }

    pub(super) fn handle_click(&mut self) {
        // Handle any clicked action from previous iteration
        if let Some(click_pos) = self.viewport.clicked_position.take() {
            // The sidebar's verbs resolve before its rows (a verb sits on a focused
            // row's line), and both before anything underneath — a click on the panel
            // focuses the sidebar the way a click on an entry does (docs/tui.md).
            if let Some((row, idx)) = self
                .sidebar_verb_hits
                .iter()
                .find(|(rect, _, _)| rect.contains(click_pos))
                .map(|(_, r, i)| (*r, *i))
            {
                self.composing = false;
                self.strip_focus = None;
                if self.sidebar_focus != Some(row) {
                    self.sidebar_return.get_or_insert(SidebarReturn::Transcript);
                    self.sidebar_focus = Some(row);
                    self.sidebar_verb_focus = 0;
                }
                self.sidebar_open = true;
                self.sidebar_verb_focus = idx;
                self.activate_sidebar_row();
                return;
            }
            if let Some(row) = self
                .sidebar_hits
                .iter()
                .find(|(rect, _)| rect.contains(click_pos))
                .map(|(_, r)| *r)
            {
                self.composing = false;
                self.strip_focus = None;
                if self.sidebar_focus != Some(row) {
                    self.sidebar_verb_focus = 0;
                }
                self.sidebar_return.get_or_insert(SidebarReturn::Transcript);
                self.sidebar_open = true;
                self.sidebar_focus = Some(row);
                return;
            }

            // The block menu sits *on* the focused unit's header row, which is
            // itself the expand action's click target — so menu items resolve
            // first, or every menu click would just expand the block
            // (docs/tui.md).
            if let Some(idx) = self
                .block_menu_hits
                .iter()
                .find(|(rect, _)| rect.contains(click_pos))
                .map(|(_, i)| *i)
            {
                self.block_menu_focus = idx;
                self.activate_block_menu();
                return;
            }

            let hit = self
                .render_cache
                .actions
                .iter()
                .position(|(rect, _)| rect.contains(click_pos));

            // Clicking a transcript entry *focuses* it — the click and the `↑`/`↓`
            // cursor address the same units, so the menu that appears is the one the
            // keyboard would have reached (docs/tui.md). Focus is all an entry
            // activation does; there is nothing to expand.
            if let Some(index) = hit
                && matches!(
                    self.render_cache.actions.get(index),
                    Some((_, TuiAction::FocusEntry(_)))
                )
            {
                self.composing = false;
                self.viewport.keyboard_cursor = super::state::KeyboardCursor::Focused {
                    action_index: index,
                };
                self.block_menu_focus = 0;
                return;
            }

            let action_opt = hit
                .and_then(|i| self.render_cache.actions.get(i).cloned())
                .map(|(_, action)| action);

            if let Some(action) = action_opt {
                // Handle SelectTheme specially (doesn't go through request thread)
                if let TuiAction::SelectTheme(theme_name) = &action {
                    // Apply theme immediately
                    let _ = self.apply_theme(theme_name);

                    // Update selected index in ThemePicker mode
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
                    handle_action(&mut self.document, &mut self.view, action);
                }
            }
        }
    }

    pub(super) fn handle_hover(&mut self) {
        if self.loading.pending_request {
            return;
        }

        use super::state::KeyboardCursor;

        // Check keyboard focus first (takes priority per spec)
        match self.viewport.keyboard_cursor {
            KeyboardCursor::Focused { action_index } => {
                if let Some((_, action)) = self.render_cache.actions.get(action_index) {
                    self.ui.debug_message = match action {
                        TuiAction::FocusEntry(_) => {
                            "navigate — ↑/↓ unit · ←/→ verb · esc compose".into()
                        }
                        TuiAction::OpenUrl(url) => format!("Open: {} (⏎ to activate)", url).into(),
                        TuiAction::SelectTheme(theme_name) => {
                            format!("Preview theme: {} (⏎ to activate)", theme_name).into()
                        }
                    };
                    return; // Keyboard focus takes priority
                }
                // Focused on invalid action_index - fall through to mouse hover
            }
            KeyboardCursor::VirtualTop | KeyboardCursor::VirtualBottom => {
                // Not focused on any link - fall through to mouse hover
            }
        }

        // No keyboard focus (or invalid focus) - show mouse hover or default message
        if self.ui.mouse_enabled
            && let Some(pos) = self.viewport.cursor_pos
            && let Some((_, action)) = self
                .render_cache
                .actions
                .iter()
                .find(|(rect, _)| rect.contains(pos))
        {
            self.ui.debug_message = match action {
                TuiAction::FocusEntry(_) => "click to focus this entry".into(),
                TuiAction::OpenUrl(url) => format!("Open: {}", url).into(),
                TuiAction::SelectTheme(theme_name) => {
                    format!("Preview theme: {}", theme_name).into()
                }
            };
        }
    }

    /// Handle scrollbar drag by calculating scroll position from mouse Y
    fn handle_scrollbar_drag(&mut self, mouse_y: u16, viewport_height: u16) {
        if let Some(cache) = self.viewport.cached_layout {
            let document_height = cache.document_height;

            // Calculate what percentage of the scrollbar was clicked
            let percentage = mouse_y as f32 / viewport_height as f32;

            // Map to scroll range
            let max_scroll = document_height.saturating_sub(viewport_height);
            let target_scroll = (percentage * max_scroll as f32).round() as u16;

            self.set_scroll_offset(target_scroll);
        }
    }

    pub(super) fn update_cursor(&mut self, terminal: &mut Terminal<impl Backend + Write>) {
        // Update cursor shape based on loading and hover state
        if self.ui.supports_cursor {
            if self.loading.pending_request {
                // Loading takes precedence - show wait cursor
                if !self.loading.was_loading {
                    set_cursor_shape(terminal.backend_mut(), "wait");
                    self.loading.was_loading = true;
                }
            } else {
                // Check for scrollbar hover/drag
                let scrollbar_hover = self.viewport.scrollbar_hovered;
                let scrollbar_drag = self.viewport.scrollbar_dragging;

                // When not loading, check hover state
                let content_hover = self
                    .viewport
                    .cursor_pos
                    .map(|pos| {
                        self.render_cache
                            .actions
                            .iter()
                            .any(|(rect, _)| rect.contains(pos))
                    })
                    .unwrap_or(false);

                let now_hovering = content_hover || scrollbar_hover;

                // Update cursor only if state changed
                if self.loading.was_loading || now_hovering != self.ui.is_hovering || scrollbar_drag
                {
                    let shape = if scrollbar_drag {
                        "grabbing"
                    } else if scrollbar_hover {
                        "grab"
                    } else if now_hovering {
                        "pointer"
                    } else {
                        "default"
                    };
                    set_cursor_shape(terminal.backend_mut(), shape);
                    self.ui.is_hovering = now_hovering;
                    self.loading.was_loading = false;
                }
            }
        }
    }
}
