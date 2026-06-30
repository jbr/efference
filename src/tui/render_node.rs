use ratatui::{buffer::Buffer, layout::Rect, style::Modifier};

use super::state::InteractiveState;
use crate::tui::document::{
    Author, DiffLineKind, DocumentNode, HeadingLevel, ShowWhen, Span, SpanStyle, TruncationLevel,
    TuiAction,
};
use crate::tui::render_document::BASELINE_LEFT_MARGIN;

// A collapsed (`Brief`) tool block reveals this many source lines before the
// disclosure triangle takes over; more than this earns a "(+N lines)" hint.
const BRIEF_LINES: usize = 8;

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

                if self.layout.pos.y >= self.viewport.scroll_offset
                    && self.layout.pos.y < self.viewport.scroll_offset + self.layout.area.height
                {
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

                if self.layout.pos.y >= self.viewport.scroll_offset
                    && self.layout.pos.y < self.viewport.scroll_offset + self.layout.area.height
                {
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

            DocumentNode::TruncatedBlock {
                header,
                resting,
                nodes,
                level,
                ..
            } => {
                // First row of this unit and the index its focus action will take —
                // captured before any rendering so the focus bar (below) can span the
                // whole block and match the keyboard cursor. Only a block with a header
                // pushes a focus action (and so can be focused).
                let block_start = self.layout.pos.y;
                let my_action_index = self.render_cache.actions.len();
                let has_action = header.is_some();

                // Source lines the body would occupy fully expanded. The transcript's
                // collapsible body is a single CodeBlock, so this is its line count;
                // counting newlines (not wrapped rows) keeps the collapse decision
                // width-independent and deterministic.
                let total_lines: usize = nodes.iter().map(node_line_count).sum();
                let collapsible = total_lines > BRIEF_LINES;
                let collapsed = collapsible && matches!(level, TruncationLevel::Brief);
                let hidden = total_lines.saturating_sub(BRIEF_LINES);

                // Leading glyph: a disclosure triangle (▶ collapsed / ▼ expanded) when
                // there's more than fits, else the block's resting glyph (an exchange's
                // `❯`/`✗`), else a neutral ▌. The whole line toggles the block.
                if let Some(header) = header {
                    self.layout.pos.x = self.layout.indent;
                    self.draw_blockquote_markers(buf);
                    let header_row = self.layout.pos.y;

                    if collapsible {
                        self.render_span(&muted_span(if collapsed { "▶ " } else { "▼ " }), buf);
                    } else if let Some(resting) = resting {
                        self.render_span(resting, buf);
                    } else {
                        self.render_span(&muted_span("▌ "), buf);
                    }
                    for span in header {
                        self.render_span(span, buf);
                    }
                    if collapsed {
                        let hint = format!("  (+{hidden} lines)");
                        self.render_span(&muted_span(&hint), buf);
                    }
                    self.layout.pos.y += 1;

                    {
                        // Every exchange is a focus unit (docs/tui.md), whether or not
                        // it's long enough to collapse — so `↑/↓` moves between *all*
                        // of them, not just the collapsible ones (the bug where focus
                        // got stuck on the only long block). `Enter` still only
                        // *expands* the collapsible ones; on a short block it's a
                        // harmless no-op (the render ignores the level when nothing is
                        // hidden). The whole header line is the click/activate target.
                        let rect = Rect::new(0, header_row, self.layout.area.width, 1);
                        self.render_cache
                            .actions
                            .push((rect, TuiAction::ExpandBlock(self.layout.node_path)));
                    }
                }

                // Body: render children, clipping code to BRIEF_LINES while collapsed.
                for (idx, child_node) in nodes.iter().enumerate() {
                    if idx > 0 {
                        self.layout.pos.y += 1;
                    }
                    let saved_path = self.layout.node_path;
                    self.layout.node_path.push(idx);

                    if collapsed {
                        self.render_node(&clip_node(child_node, BRIEF_LINES), buf);
                    } else {
                        self.render_node(child_node, buf);
                    }

                    self.layout.node_path = saved_path;
                }

                // Focus bar: an obvious glyph marker down the focused unit's full
                // height (docs/tui.md), painted after the body so it spans every row.
                // Glyph-based, not reversed-video — the latter is invisible on themes
                // whose fg/bg are close, and never shows in the no-color snapshot grid.
                // Only collapsible blocks carry a focus action today (the per-turn
                // focusability is a later step), so only they can be focused.
                let is_focused = has_action
                    && matches!(
                        self.viewport.keyboard_cursor,
                        super::state::KeyboardCursor::Focused { action_index }
                            if action_index == my_action_index
                    );
                if is_focused {
                    let style = self.style(SpanStyle::UserAccent);
                    for row in block_start..self.layout.pos.y {
                        if row >= self.viewport.scroll_offset
                            && row < self.viewport.scroll_offset + self.layout.area.height
                        {
                            self.write_text(buf, row, 0, "▌", self.layout.area, style);
                        }
                    }
                }
            }

            DocumentNode::Turn { author, blocks } => {
                let start_row = self.layout.pos.y;

                // Every block is a focus unit (docs/tui.md), in the flat model: a
                // turn's *leading prose run* (the blocks before its first exchange) is
                // one unit, and each exchange is its own. So `↑/↓` stops on the prose
                // ("Let me check…") and on the bash/search/list calls alike, instead of
                // skipping prose. The prose unit's action is pushed *before* the blocks
                // render, so its index precedes the exchanges' — keeping navigation
                // order equal to visual order. Activating it is a no-op (prose has
                // nothing to expand); it exists purely to be a focus target.
                let prose_len = blocks
                    .iter()
                    .take_while(|b| !matches!(b, DocumentNode::TruncatedBlock { .. }))
                    .count();
                let prose_action_index = self.render_cache.actions.len();
                if prose_len > 0 {
                    let rect = Rect::new(0, start_row, self.layout.area.width, 1);
                    self.render_cache
                        .actions
                        .push((rect, TuiAction::ExpandBlock(self.layout.node_path)));
                }

                let mut prose_end = start_row;
                for (idx, block) in blocks.iter().enumerate() {
                    // Blank line between blocks, mirroring the top-level spacing.
                    if idx > 0 {
                        self.layout.pos.y += 1;
                    }
                    let saved_path = self.layout.node_path;
                    self.layout.node_path.push(idx);
                    self.render_node(block, buf);
                    self.layout.node_path = saved_path;
                    if idx + 1 == prose_len {
                        prose_end = self.layout.pos.y;
                    }
                }

                // Focus bar for the prose unit (the exchanges paint their own, above).
                if prose_len > 0
                    && matches!(
                        self.viewport.keyboard_cursor,
                        super::state::KeyboardCursor::Focused { action_index }
                            if action_index == prose_action_index
                    )
                {
                    let style = self.style(SpanStyle::UserAccent);
                    for row in start_row..prose_end {
                        if row >= self.viewport.scroll_offset
                            && row < self.viewport.scroll_offset + self.layout.area.height
                        {
                            self.write_text(buf, row, 0, "▌", self.layout.area, style);
                        }
                    }
                }

                // Authorship is structural: the user's turn carries an accent gutter
                // down the reserved left margin; the assistant is the unmarked
                // default voice. Painted after the blocks so it spans their rows.
                if matches!(author, Author::User) {
                    let gutter_col = BASELINE_LEFT_MARGIN.saturating_sub(2);
                    let style = self.style(SpanStyle::UserAccent);
                    for row in start_row..self.layout.pos.y {
                        if row >= self.viewport.scroll_offset
                            && row < self.viewport.scroll_offset + self.layout.area.height
                        {
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

/// A muted span for accordion chrome — the disclosure triangle / block marker and
/// the "(+N lines)" hint.
fn muted_span(text: &str) -> Span<'static> {
    Span {
        text: text.to_string().into(),
        style: SpanStyle::Comment,
        action: None,
    }
}

/// Source-line count a node would occupy fully expanded — the input to the
/// collapse decision. Width-independent (counts newlines, not wrapped rows).
fn node_line_count(node: &DocumentNode<'_>) -> usize {
    match node {
        DocumentNode::CodeBlock { code, .. } => code.lines().count().max(1),
        DocumentNode::Diff { lines } => lines.len().max(1),
        DocumentNode::List { items, .. } => items.len(),
        _ => 1,
    }
}

/// A copy of `node` clipped to its first `max_lines` source lines (the collapsed
/// view). Only code blocks clip — the transcript's collapsible body is always one.
fn clip_node<'a>(node: &DocumentNode<'a>, max_lines: usize) -> DocumentNode<'a> {
    match node {
        DocumentNode::CodeBlock { lang, code } => {
            let clipped = code.lines().take(max_lines).collect::<Vec<_>>().join("\n");
            DocumentNode::CodeBlock {
                lang: lang.clone(),
                code: clipped.into(),
            }
        }
        DocumentNode::Diff { lines } => DocumentNode::Diff {
            lines: lines.iter().take(max_lines).cloned().collect(),
        },
        other => other.clone(),
    }
}
