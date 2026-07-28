use ratatui::{buffer::Buffer, layout::Rect, style::Modifier};

use super::state::InteractiveState;
use crate::tui::document::{
    DiffLineKind, DocumentNode, EntryKind, HeadingLevel, ShowWhen, Span, SpanStyle, TuiAction,
};
use crate::tui::render_document::BASELINE_LEFT_MARGIN;

impl<'a> InteractiveState<'a> {
    /// Draw all active blockquote markers at the current row
    pub(super) fn draw_blockquote_markers(&mut self, buf: &mut Buffer) {
        let quote_style = self.theme.muted_style;
        for &marker_x in &self.layout.blockquote_markers {
            self.write_text(
                buf,
                self.layout.pos.y,
                marker_x,
                "  ┃ ",
                self.layout.area,
                quote_style,
            );
        }
    }

    /// Render a single node
    pub(super) fn render_node(&mut self, node: &DocumentNode<'a>, buf: &mut Buffer) {
        match node {
            DocumentNode::Paragraph { spans } => {
                // Block element: unconditionally position at indent
                self.layout.pos.x = self.layout.indent;
                // Draw blockquote markers if we're inside a blockquote
                self.draw_blockquote_markers(buf);
                for span in spans {
                    self.render_span(span, buf);
                }

                // Block element: increment y when done
                self.layout.pos.y += 1;
            }

            DocumentNode::Metadata { fields } => {
                // Render each field on its own line: bold label + styled value.
                for field in fields {
                    self.layout.pos.x = self.layout.indent;
                    self.draw_blockquote_markers(buf);

                    let label_span = Span::plain(format!("{}: ", field.label));
                    self.render_span_with_modifier(&label_span, Modifier::BOLD, buf);
                    for span in &field.value {
                        self.render_span(span, buf);
                    }
                    self.layout.pos.y += 1;
                }
            }

            DocumentNode::Heading { level, spans } => {
                // Block element: unconditionally position at indent
                self.layout.pos.x = self.layout.indent;
                // Draw blockquote markers if we're inside a blockquote
                self.draw_blockquote_markers(buf);

                // Render heading spans (bold)
                for span in spans {
                    self.render_span_with_modifier(span, Modifier::BOLD, buf);
                }

                // New line after heading
                self.layout.pos.y += 1;

                // Add decorative underline (respecting left margin)
                let underline_char = match level {
                    HeadingLevel::Title => '═',
                    HeadingLevel::Section => '┄',
                };

                // Draw blockquote markers on underline row
                self.draw_blockquote_markers(buf);

                if self.row_visible(self.layout.pos.y) {
                    for c in self.layout.indent..self.layout.area.width {
                        buf.cell_mut((c, self.layout.pos.y - self.viewport.scroll_offset))
                            .unwrap()
                            .set_char(underline_char);
                    }
                }

                // Block element: increment y when done
                self.layout.pos.y += 1;
            }

            DocumentNode::List { items, start } => {
                for (item_idx, item) in items.iter().enumerate() {
                    // Add blank line between list items
                    if item_idx > 0 {
                        // Draw blockquote markers on blank line
                        self.draw_blockquote_markers(buf);
                        self.layout.pos.y += 1;
                    }

                    // Block element: unconditionally position at indent
                    self.layout.pos.x = self.layout.indent;
                    // Draw blockquote markers before the item marker
                    self.draw_blockquote_markers(buf);

                    // Ordered lists number from `start`; bulleted lists use a nesting
                    // glyph. The marker width varies (e.g. "  1. "), so advance by its
                    // actual width rather than a fixed 4 columns.
                    let marker = match start {
                        Some(n) => format!("  {}. ", n + item_idx as u64),
                        None => format!("  {} ", crate::tui::bullet_for_indent(self.layout.indent)),
                    };
                    let marker_width = marker.chars().count() as u16;
                    let marker_style = self.theme.muted_style;
                    self.write_text(
                        buf,
                        self.layout.pos.y,
                        self.layout.pos.x,
                        &marker,
                        self.layout.area,
                        marker_style,
                    );
                    self.layout.pos.x += marker_width;

                    // Save indent and update for content
                    let saved_indent = self.layout.indent;
                    self.layout.indent = self.layout.pos.x;

                    // Render content nodes
                    // Note: no blank line spacing between blocks in list items - keep them compact
                    for (content_idx, content_node) in item.content.iter().enumerate() {
                        // Save path, update it, render, restore
                        let saved_path = self.layout.node_path;
                        self.layout.node_path.push(item_idx);
                        self.layout.node_path.push(content_idx);

                        self.render_node(content_node, buf);

                        self.layout.node_path = saved_path;
                    }

                    // Restore indent
                    self.layout.indent = saved_indent;
                }
                // Container: children handle their own spacing
            }

            DocumentNode::Section { title, nodes } => {
                if let Some(title_spans) = title {
                    // Block element: unconditionally position at indent
                    self.layout.pos.x = self.layout.indent;

                    for span in title_spans {
                        self.render_span_with_modifier(span, Modifier::BOLD, buf);
                    }

                    // Add blank line after section title
                    self.layout.pos.y += 1;
                    self.layout.pos.y += 1;
                }

                for (idx, child_node) in nodes.iter().enumerate() {
                    // Add blank line between consecutive blocks
                    if idx > 0 {
                        self.layout.pos.y += 1;
                    }

                    // Save and update path
                    let saved_path = self.layout.node_path;
                    self.layout.node_path.push(idx);

                    self.render_node(child_node, buf);

                    self.layout.node_path = saved_path;
                }
                // Container: children handle their own spacing
            }

            DocumentNode::CodeBlock { lang, code } => {
                // Block element: unconditionally position at indent
                self.layout.pos.x = self.layout.indent;

                self.render_code_block(lang.as_deref(), code, buf);

                // Block element: increment y when done
                self.layout.pos.y += 1;
            }

            DocumentNode::NumberedCode { lang, lines } => {
                // Block element: unconditionally position at indent
                self.layout.pos.x = self.layout.indent;

                self.render_numbered_code(lang.as_deref(), lines, buf);

                // Block element: increment y when done
                self.layout.pos.y += 1;
            }

            DocumentNode::GeneratedCode { spans } => {
                // Block element: unconditionally position at indent
                self.layout.pos.x = self.layout.indent;
                // Draw blockquote markers if we're inside a blockquote
                self.draw_blockquote_markers(buf);

                // Render spans inline (flows with current position)
                for span in spans {
                    self.render_span(span, buf);
                }

                // Block element: increment y when done
                self.layout.pos.y += 1;
            }

            DocumentNode::HorizontalRule => {
                // Block element: unconditionally position at indent
                self.layout.pos.x = self.layout.indent;
                // Draw blockquote markers if we're inside a blockquote
                self.draw_blockquote_markers(buf);

                if self.row_visible(self.layout.pos.y) {
                    let rule_style = self.theme.muted_style;
                    // Use a decorative pattern: ─── • ───
                    let pattern = ['─', '─', '─', ' ', '•', ' '];
                    for c in 0..self.layout.area.width {
                        let ch = pattern[(c as usize) % pattern.len()];
                        if let Some(cell) =
                            buf.cell_mut((c, self.layout.pos.y - self.viewport.scroll_offset))
                        {
                            cell.set_char(ch);
                            cell.set_style(rule_style);
                        }
                    }
                }

                // Block element: increment y when done
                self.layout.pos.y += 1;
            }

            DocumentNode::BlockQuote { nodes } => {
                // Add this blockquote's marker position to the stack
                let marker_x = self.layout.indent;
                self.layout.blockquote_markers.push(marker_x);

                // Update indent to account for marker
                let saved_indent = self.layout.indent;
                self.layout.indent += 4; // "  ┃ " takes 4 columns

                for (idx, child_node) in nodes.iter().enumerate() {
                    // Add blank line between consecutive blocks
                    if idx > 0 {
                        // Draw all blockquote markers on the blank line
                        self.draw_blockquote_markers(buf);
                        self.layout.pos.y += 1;
                    }

                    let saved_path = self.layout.node_path;
                    self.layout.node_path.push(idx);
                    self.render_node(child_node, buf);
                    self.layout.node_path = saved_path;
                }

                // Restore indent and pop marker
                self.layout.indent = saved_indent;
                self.layout.blockquote_markers.pop();

                // Container: children handle their own spacing
            }

            DocumentNode::Table { header, rows } => {
                // Block element: unconditionally position at indent
                self.layout.pos.x = self.layout.indent;

                self.render_table(header.as_deref(), rows, buf);

                // Block element: increment y when done
                self.layout.pos.y += 1;
            }

            DocumentNode::Diff { lines } => {
                // One styled line per diff line; the leading ` `/`+`/`-` is kept so
                // columns align. Added → green, Removed → red, Hunk → muted,
                // Context → default foreground.
                for line in lines {
                    self.layout.pos.x = self.layout.indent;
                    self.draw_blockquote_markers(buf);
                    let style = match line.kind {
                        DiffLineKind::Added => SpanStyle::Added,
                        DiffLineKind::Removed => SpanStyle::Removed,
                        DiffLineKind::Hunk => SpanStyle::Comment,
                        DiffLineKind::Context => SpanStyle::Plain,
                    };
                    self.render_span(
                        &Span {
                            text: line.text.clone().into(),
                            style,
                            action: None,
                        },
                        buf,
                    );
                    self.layout.pos.y += 1;
                }
            }

            DocumentNode::Entry {
                kind,
                marker,
                header,
                blocks,
                key,
            } => {
                // First row of this entry and the index its focus action takes —
                // captured before anything renders, so the focus bar (below) spans the
                // whole entry and the index matches the keyboard cursor. The action is
                // pushed *first* so navigation order equals visual order even when a
                // header span carries a link of its own.
                let entry_start = self.layout.pos.y;
                let my_action_index = self.render_cache.actions.len();
                if kind.is_focusable() {
                    // The whole first row is the focus/click target. Focus is all it
                    // is: an entry renders whole, so there is nothing to expand
                    // (docs/tui.md).
                    let rect = Rect::new(0, entry_start, self.layout.area.width, 1);
                    self.render_cache
                        .actions
                        .push((rect, TuiAction::FocusEntry(self.layout.node_path)));
                }
                let is_focused = kind.is_focusable()
                    && matches!(
                        self.viewport.keyboard_cursor,
                        super::state::KeyboardCursor::Focused { action_index }
                            if action_index == my_action_index
                    );

                // The header row: the entry's marker glyph, its header spans, and —
                // when focused — the block menu trailing them.
                if let Some(header) = header {
                    self.layout.pos.x = self.layout.indent;
                    self.draw_blockquote_markers(buf);
                    let header_row = self.layout.pos.y;

                    if let Some(marker) = marker {
                        self.render_span(marker, buf);
                    }
                    for span in header {
                        self.render_span(span, buf);
                    }
                    // The block menu (docs/tui.md): the focused unit's own verbs,
                    // trailing its header text — appending clobbers no header
                    // columns and reflows nothing as focus moves.
                    if is_focused {
                        let key = key.as_deref();
                        let items = super::block_menu::menu_items(
                            *kind,
                            key,
                            key.is_some_and(|k| self.view.is_raw(k)),
                        );
                        let col = self.layout.pos.x + 2;
                        self.render_block_menu(buf, &items, header_row, col, self.layout.area);
                    }
                    self.layout.pos.y += 1;
                }

                // Body: every child, whole. Nothing here consults a line budget —
                // if the agent saw it, the operator sees it (docs/positions.md).
                for (idx, child_node) in blocks.iter().enumerate() {
                    if idx > 0 {
                        self.layout.pos.y += 1;
                    }
                    let saved_path = self.layout.node_path;
                    self.layout.node_path.push(idx);
                    self.render_node(child_node, buf);
                    self.layout.node_path = saved_path;
                }

                // Focus bar: an obvious glyph marker down the focused unit's full
                // height (docs/tui.md), painted after the body so it spans every row.
                // Glyph-based, not reversed-video — the latter is invisible on themes
                // whose fg/bg are close, and never shows in the no-color snapshot grid.
                if is_focused {
                    let style = self.style(SpanStyle::UserAccent);
                    for row in entry_start..self.layout.pos.y {
                        if self.row_visible(row) {
                            self.write_text(buf, row, 0, "▌", self.layout.area, style);
                        }
                    }
                }

                // Authorship is structural: what the operator said carries an accent
                // gutter down the reserved left margin; the agent's own voice is the
                // unmarked default. Painted after the body so it spans its rows. A
                // tool result is `role: user` on the wire but is not the operator
                // speaking, and is a kind of its own — so it never lands here.
                if matches!(kind, EntryKind::UserMessage) {
                    let gutter_col = BASELINE_LEFT_MARGIN.saturating_sub(2);
                    let style = self.style(SpanStyle::UserAccent);
                    for row in entry_start..self.layout.pos.y {
                        if self.row_visible(row) {
                            self.write_text(buf, row, gutter_col, "┃", self.layout.area, style);
                        }
                    }
                }
            }

            DocumentNode::Conditional { show_when, nodes } => {
                // Transparent container: doesn't add its own newlines
                // Interactive renderer is always in interactive mode
                let should_show = match show_when {
                    ShowWhen::Always => true,
                    ShowWhen::Interactive => true,
                    ShowWhen::NonInteractive => false,
                };

                if should_show {
                    for (idx, node) in nodes.iter().enumerate() {
                        // Add blank line between consecutive blocks
                        if idx > 0 {
                            self.layout.pos.y += 1;
                        }
                        self.render_node(node, buf);
                    }
                }
                // Transparent container: no additional spacing
            }
        }
    }
}
