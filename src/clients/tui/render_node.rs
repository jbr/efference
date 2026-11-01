use ratatui::{buffer::Buffer, style::Modifier};

use super::state::InteractiveState;
use crate::clients::tui::document::{
    DiffLine, DiffLineKind, DocumentNode, HeadingLevel, ListItem, MetadataField, ShowWhen, Span,
    SpanStyle,
};

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
            DocumentNode::Dimmed { nodes } => self.render_dimmed(nodes, buf),
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

            // Each line renders exactly as a `Paragraph` would; what differs is that
            // nothing separates them, because they were never separate blocks.
            DocumentNode::Lines { lines } => {
                for spans in lines {
                    self.layout.pos.x = self.layout.indent;
                    self.draw_blockquote_markers(buf);
                    for span in spans {
                        self.render_span(span, buf);
                    }
                    self.layout.pos.y += 1;
                }
            }

            DocumentNode::Metadata { fields } => self.render_metadata(fields, buf),

            DocumentNode::Heading { level, spans } => self.render_heading(*level, spans, buf),

            DocumentNode::List { items, start } => self.render_list(items, *start, buf),

            DocumentNode::Section { title, nodes } => {
                self.render_section(title.as_deref(), nodes, buf);
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

            DocumentNode::Image {
                seq,
                idx,
                media_type,
                width,
                height,
            } => {
                // Block element: position at indent; the image renderer owns the
                // cursor advance (its height is the reserved footprint).
                self.layout.pos.x = self.layout.indent;
                self.render_image_node(*seq, *idx, media_type, *width, *height, buf);
            }

            DocumentNode::HorizontalRule => self.render_horizontal_rule(buf),

            DocumentNode::BlockQuote { nodes } => self.render_block_quote(nodes, buf),

            DocumentNode::Table { header, rows } => {
                // Block element: unconditionally position at indent
                self.layout.pos.x = self.layout.indent;

                self.render_table(header.as_deref(), rows, buf);

                // Block element: increment y when done
                self.layout.pos.y += 1;
            }

            DocumentNode::Diff { lines } => self.render_diff(lines, buf),

            DocumentNode::CallItem { .. } => self.render_call_item(node, buf),

            DocumentNode::ThinkingItem { key, words, entry } => {
                self.render_thinking_item(key, *words, entry, buf);
            }

            DocumentNode::Captioned { caption, nodes } => {
                self.render_captioned(caption, nodes, buf);
            }

            DocumentNode::Signature { view, arriving } => {
                self.render_signature(view, *arriving, buf);
            }

            DocumentNode::Entry {
                kind,
                marker,
                header,
                blocks,
                key,
            } => self.render_entry(
                *kind,
                marker.as_ref(),
                header.as_deref(),
                blocks,
                key.as_deref(),
                buf,
            ),

            DocumentNode::Conditional { show_when, nodes } => {
                self.render_conditional(*show_when, nodes, buf);
            }
        }
    }

    /// Structured key-value metadata: each field on its own line, a bold label and its
    /// styled value.
    fn render_metadata(&mut self, fields: &[MetadataField<'a>], buf: &mut Buffer) {
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

    /// A transparent container that shows its children only in the modes `show_when` names.
    fn render_conditional(
        &mut self,
        show_when: ShowWhen,
        nodes: &[DocumentNode<'a>],
        buf: &mut Buffer,
    ) {
        // Transparent container: doesn't add its own newlines
        // Interactive renderer is always in interactive mode
        let should_show = match show_when {
            ShowWhen::Always | ShowWhen::Interactive => true,
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

    /// Render the children in the dimmed register (docs/tui.md).
    fn render_dimmed(&mut self, nodes: &[DocumentNode<'a>], buf: &mut Buffer) {
        // Set the register for the subtree and restore it after: nesting is
        // possible in principle and saving the prior value keeps it sane,
        // where an unconditional `= false` would un-dim the rest of an outer
        // dimmed block.
        let outer = self.dim_register;
        self.dim_register = true;
        // Children are blocks, separated by a blank row — the same
        // spacing `Entry` gives its own, so a dimmed subtree reads like
        // any other block sequence rather than running together.
        for (idx, child) in nodes.iter().enumerate() {
            if idx > 0 {
                self.layout.pos.y += 1;
            }
            let saved_path = self.layout.node_path;
            self.layout.node_path.push(idx);
            self.render_node(child, buf);
            self.layout.node_path = saved_path;
        }
        self.dim_register = outer;
    }

    /// A heading: its spans in bold, then a decorative underline.
    fn render_heading(&mut self, level: HeadingLevel, spans: &[Span<'a>], buf: &mut Buffer) {
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

    /// A list: one marker per item, the item's blocks indented past it.
    fn render_list(&mut self, items: &[ListItem<'a>], start: Option<u64>, buf: &mut Buffer) {
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
                None => format!(
                    "  {} ",
                    crate::clients::tui::bullet_for_indent(self.layout.indent)
                ),
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

    /// A section: an optional bold title, then its blocks a blank row apart.
    fn render_section(
        &mut self,
        title: Option<&[Span<'a>]>,
        nodes: &[DocumentNode<'a>],
        buf: &mut Buffer,
    ) {
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

    /// A horizontal rule: a repeating pattern across the area's width.
    fn render_horizontal_rule(&mut self, buf: &mut Buffer) {
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

    /// A block quote: its marker joins the stack and the indent moves past it for the
    /// children.
    fn render_block_quote(&mut self, nodes: &[DocumentNode<'a>], buf: &mut Buffer) {
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

    /// A diff: one styled row per line.
    fn render_diff(&mut self, lines: &[DiffLine], buf: &mut Buffer) {
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

    /// A block with its caption directly above it.
    fn render_captioned(
        &mut self,
        caption: &[Span<'a>],
        nodes: &[DocumentNode<'a>],
        buf: &mut Buffer,
    ) {
        self.layout.pos.x = self.layout.indent;
        self.draw_blockquote_markers(buf);
        for span in caption {
            self.render_span(span, buf);
        }
        self.layout.pos.y += 1;
        for (idx, child) in nodes.iter().enumerate() {
            if idx > 0 {
                self.layout.pos.y += 1;
            }
            let saved_path = self.layout.node_path;
            self.layout.node_path.push(idx);
            self.render_node(child, buf);
            self.layout.node_path = saved_path;
        }
    }
}
