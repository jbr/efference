//! Mouse events onto document actions: hover, click, drag, and scroll (docs/tui.md).

use std::io::Write;

use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
use ratatui::{Terminal, layout::Position, prelude::Backend};

use super::SidebarReturn;
use super::events::handle_action;
use super::utils::set_cursor_shape;
use crate::clients::tui::document::TuiAction;
use crate::clients::tui::render_context::RenderContext;

use super::UiMode;

impl<'a> super::InteractiveState<'a> {
    pub(super) fn handle_mouse_event(
        &mut self,
        mouse_event: MouseEvent,
        terminal: &Terminal<impl Backend>,
    ) {
        #[cfg(feature = "dev")]
        if matches!(&self.ui_mode, UiMode::Sessions(menu) if menu.form.is_some()) {
            return;
        }
        if !self.ui.mouse_enabled {
            return;
        }

        match mouse_event {
            MouseEvent {
                kind: MouseEventKind::Moved,
                column,
                row,
                ..
            } => self.pointer_moved(column, row, terminal),

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
            } => self.left_pressed(column, row, modifiers, terminal),

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
                let top = self.viewport.doc_origin.y;
                let content_height = size
                    .height
                    .saturating_sub(
                        1 + self.viewport.input_height + self.viewport.above_input_height,
                    )
                    .saturating_sub(top);
                self.handle_scrollbar_drag(row.saturating_sub(top), content_height);
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

    /// A click inside the open session drawer, in screen coordinates (the drawer
    /// is chrome, recorded at absolute positions). Verbs resolve before rows (a
    /// verb sits on a focused row's line); a click on neither leaves the drawer
    /// open and focused.
    fn click_drawer(&mut self, click_pos: Position) {
        self.composing = false;
        self.strip_focus = None;
        self.sidebar_return.get_or_insert(SidebarReturn::Transcript);
        if let Some((row, idx)) = self
            .sidebar_verb_hits
            .iter()
            .find(|(rect, _, _)| rect.contains(click_pos))
            .map(|(_, r, i)| (*r, *i))
        {
            self.sidebar_focus = Some(row);
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
            if self.sidebar_focus != Some(row) {
                self.sidebar_verb_focus = 0;
            }
            self.sidebar_focus = Some(row);
        }
    }

    /// The header item whose column span holds `column`, as of the last frame.
    fn header_item_at(&self, column: u16) -> Option<super::render_header::HeaderItem> {
        self.viewport
            .header_hits
            .iter()
            .find(|(x0, x1, _)| column >= *x0 && column < *x1)
            .map(|(_, _, item)| *item)
    }

    fn activate_header_item(&mut self, item: super::render_header::HeaderItem) {
        match item {
            // The drawer takes the keys while open, so the composer gives up focus
            // first, as it does when Tab enters from the transcript.
            super::render_header::HeaderItem::Session => {
                self.composing = false;
                self.toggle_sidebar();
            }
        }
    }

    /// Open a folded item whole, or fold it back (docs/tui.md, "Detail by
    /// recency"). Expansion is render state, so the document is laid out again
    /// rather than re-projected.
    pub(super) fn toggle_expanded(&mut self, key: String) {
        if !self.expanded.remove(&key) {
            self.expanded.insert(key);
        }
        self.document.invalidate_all();
        self.dirty = true;
    }

    /// The focus action of the item whose rows hold document row `row`: its first
    /// entry's, the first action recorded inside it.
    fn item_focus_target(&self, row: u16) -> Option<usize> {
        let &(start, end) = self
            .render_cache
            .items
            .iter()
            .find(|(start, end)| row >= *start && row < *end)?;
        self.render_cache.actions.iter().position(|(rect, action)| {
            rect.y >= start
                && rect.y < end
                && matches!(action, TuiAction::FocusEntry(_) | TuiAction::ExpandItem(_))
        })
    }

    pub(super) fn handle_click(&mut self) {
        // Handle any clicked action from previous iteration
        if let Some(click_pos) = self.viewport.clicked_position.take() {
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

            // A folded item opens whole on a click (docs/tui.md, "Detail by recency").
            if let Some(index) = hit
                && let Some((_, TuiAction::ExpandItem(key))) = self.render_cache.actions.get(index)
            {
                let key = key.to_string();
                self.toggle_expanded(key);
                return;
            }

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

            // A click on any row of a call or thinking item focuses it, as a click
            // anywhere on the web client's call card acts on the call — not only a
            // click on its first row (docs/tui.md).
            if hit.is_none()
                && let Some(index) = self.item_focus_target(click_pos.y)
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
                    handle_action(&mut self.document, &mut self.view, &action);
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
                        TuiAction::OpenUrl(url) => format!("Open: {url} (⏎ to activate)").into(),
                        TuiAction::SelectTheme(theme_name) => {
                            format!("Preview theme: {theme_name} (⏎ to activate)").into()
                        }
                        TuiAction::ExpandItem(_) => "⏎ show it whole".into(),
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
                TuiAction::OpenUrl(url) => format!("Open: {url}").into(),
                TuiAction::SelectTheme(theme_name) => format!("Preview theme: {theme_name}").into(),
                TuiAction::ExpandItem(_) => "click to show it whole".into(),
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
                let content_hover = self.viewport.cursor_pos.is_some_and(|pos| {
                    self.render_cache
                        .actions
                        .iter()
                        .any(|(rect, _)| rect.contains(pos))
                });

                // A strip item, the header's button, and a call or thinking item
                // are all click targets, so the pointer says so over them.
                let chrome_hover =
                    self.viewport.strip_hover.is_some() || self.viewport.header_hovered.is_some();
                let item_hover = self.viewport.cursor_pos.is_some_and(|pos| {
                    self.render_cache
                        .items
                        .iter()
                        .any(|(start, end)| pos.y >= *start && pos.y < *end)
                });
                let now_hovering = content_hover || scrollbar_hover || chrome_hover || item_hover;

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

mod pointer;
