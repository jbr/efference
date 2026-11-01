//! Drawing a transcript entry and a call's signature (docs/tui.md).

use ratatui::{buffer::Buffer, layout::Rect};

use super::state::InteractiveState;
use crate::clients::tui::document::{DocumentNode, EntryKind, Span, SpanStyle, TuiAction};
use crate::clients::tui::render_document::BASELINE_LEFT_MARGIN;

impl<'a> InteractiveState<'a> {
    /// One transcript entry: the focus target, the header row with its block menu when
    /// focused, the body, and the focus bar and authorship gutter painted over its rows.
    pub(super) fn render_entry(
        &mut self,
        kind: EntryKind,
        marker: Option<&Span<'a>>,
        header: Option<&[Span<'a>]>,
        blocks: &[DocumentNode<'a>],
        key: Option<&str>,
        buf: &mut Buffer,
    ) {
        // First row of this entry and the index its focus action takes —
        // captured before anything renders, so the focus bar (below) spans the
        // whole entry and the index matches the keyboard cursor. The action is
        // pushed *first* so navigation order equals visual order even when a
        // header span carries a link of its own.
        let entry_start = self.layout.pos.y;
        let my_action_index = self.render_cache.actions.len();
        if kind.is_focusable() {
            // The whole first row is the focus/click target. Focus is all it
            // is: a folded item carries its own expand target (docs/tui.md,
            // "Detail by recency").
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
                let items =
                    super::block_menu::menu_items(kind, key, self.view.is_flipped(kind, key));
                let col = self.layout.pos.x + 2;
                self.render_block_menu(buf, &items, header_row, col, self.layout.area);
            }
            self.layout.pos.y += 1;
        }

        // Body: every child, whole. A folded item cuts it with a clip from
        // outside (docs/tui.md, "Detail by recency"); nothing here does.
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

    /// A call's signature at the width it has (docs/tui.md): the prompt, then the rows
    /// [`layout_signature`](crate::clients::tui::projection::signature::layout_signature) breaks
    /// it into, continuation rows hung under the first. An argument too long to sit in
    /// it at this width renders beneath it, under its key, as a plain block.
    pub(super) fn render_signature(
        &mut self,
        view: &crate::clients::tui::projection::signature::CallView,
        arriving: bool,
        buf: &mut Buffer,
    ) {
        use crate::clients::tui::projection::signature::{TokenKind, layout_signature};
        const PROMPT: &str = "› ";
        let hang = PROMPT.chars().count() as u16;
        let columns = self
            .layout
            .area
            .width
            .saturating_sub(self.layout.indent + hang + 1) as usize;
        let layout = layout_signature(view, Some(columns));
        let span = |text: &str, style: SpanStyle| Span {
            text: text.to_string().into(),
            style,
            action: None,
        };
        let row_count = layout.rows.len();
        for (i, row) in layout.rows.iter().enumerate() {
            self.layout.pos.x = self.layout.indent;
            self.draw_blockquote_markers(buf);
            if i == 0 {
                self.render_span(&span(PROMPT, SpanStyle::UserAccent), buf);
            } else {
                self.layout.pos.x += hang;
            }
            self.layout.pos.x += row.indent as u16;
            // A value that runs past the width wraps hung under its own row's start,
            // the way the web client's indented row wraps, not back at the margin.
            let outer_indent = std::mem::replace(&mut self.layout.indent, self.layout.pos.x);
            let last_row = i + 1 == row_count;
            let token_count = row.tokens.len();
            for (j, token) in row.tokens.iter().enumerate() {
                // Still arriving: the ellipsis goes inside the closing paren.
                if arriving && last_row && !layout.shell && j + 1 == token_count {
                    let after_open = j > 0 && row.tokens[j - 1].text == "(";
                    let ellipsis = if after_open { "…" } else { ", …" };
                    self.render_span(&span(ellipsis, SpanStyle::Comment), buf);
                }
                let (text, style) = match token.kind {
                    TokenKind::Server => (token.text.clone(), SpanStyle::Comment),
                    TokenKind::Tool => (token.text.clone(), SpanStyle::FunctionName),
                    TokenKind::Sep => (token.text.clone(), SpanStyle::Punctuation),
                    TokenKind::Key => (token.text.clone(), SpanStyle::FieldName),
                    TokenKind::Value | TokenKind::Target => {
                        (token.text.clone(), SpanStyle::InlineCode)
                    }
                    TokenKind::Tag => (format!("  {}", token.text), SpanStyle::Comment),
                };
                self.render_span(&span(&text, style), buf);
            }
            if arriving && last_row && layout.shell {
                self.render_span(&span(" …", SpanStyle::Comment), buf);
            }
            self.layout.indent = outer_indent;
            self.layout.pos.y += 1;
        }
        for arg in &layout.pulled {
            self.layout.pos.y += 1;
            let text = match &arg.value {
                serde_json::Value::String(text) => text.clone(),
                other => crate::clients::tui::projection::signature::flat_value(other),
            };
            self.render_node(
                &DocumentNode::Captioned {
                    caption: vec![span(&arg.key, SpanStyle::Comment)],
                    nodes: vec![DocumentNode::CodeBlock {
                        lang: None,
                        code: text.into(),
                    }],
                },
                buf,
            );
        }
    }
}
