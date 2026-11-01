//! The pieces of a code block's box (docs/tui.md): its top and bottom borders, and the
//! highlighting its lines are drawn with.

use ratatui::{buffer::Buffer, style::Style};

use super::*;

impl<'a> InteractiveState<'a> {
    /// The top border row, with the language label on its right when there is one:
    /// ╭─────❬rust❭─╮. Moves the cursor past the row.
    pub(super) fn draw_top_border(
        &mut self,
        buf: &mut Buffer,
        border_col: u16,
        border_width: u16,
        lang_label: Option<&str>,
        border_style: Style,
    ) {
        let label_display_width = lang_label.map_or(0, |l| l.chars().count());
        if self.row_visible(self.layout.pos.y) {
            self.write_text(
                buf,
                self.layout.pos.y,
                border_col,
                "╭",
                self.layout.area,
                border_style,
            );

            // Calculate position for language label (right side, with one dash before corner)
            // Label ends at border_width - 2 (leaving space for ─╮). With no label the
            // whole top is dashes, drawn by the second loop below.
            let label_start = match &lang_label {
                Some(_) => border_col + border_width.saturating_sub(label_display_width as u16 + 2),
                None => border_col + 1,
            };

            // Draw left dashes (up to label)
            for i in 1..label_start.saturating_sub(border_col) {
                self.write_text(
                    buf,
                    self.layout.pos.y,
                    border_col + i,
                    "─",
                    self.layout.area,
                    border_style,
                );
            }

            // Draw language label
            if let Some(lang_label) = &lang_label {
                self.write_text(
                    buf,
                    self.layout.pos.y,
                    label_start,
                    lang_label,
                    self.layout.area,
                    border_style,
                );
            }

            // Draw dashes from end of label to corner
            // The label takes label_display_width columns, so next position is label_start + label_display_width
            let label_end_col = label_start + label_display_width as u16;
            for i in label_end_col..border_col + border_width.saturating_sub(1) {
                self.write_text(
                    buf,
                    self.layout.pos.y,
                    i,
                    "─",
                    self.layout.area,
                    border_style,
                );
            }

            // Draw corner
            self.write_text(
                buf,
                self.layout.pos.y,
                border_col + border_width.saturating_sub(1),
                "╮",
                self.layout.area,
                border_style,
            );
        }
        self.layout.pos.y += 1;
    }

    /// The bottom border row: ╰─────╯. Moves the cursor past the row.
    pub(super) fn draw_bottom_border(
        &mut self,
        buf: &mut Buffer,
        border_col: u16,
        border_width: u16,
        border_style: Style,
    ) {
        if self.row_visible(self.layout.pos.y) {
            self.write_text(
                buf,
                self.layout.pos.y,
                border_col,
                "╰",
                self.layout.area,
                border_style,
            );
            for i in 1..border_width.saturating_sub(1) {
                self.write_text(
                    buf,
                    self.layout.pos.y,
                    border_col + i,
                    "─",
                    self.layout.area,
                    border_style,
                );
            }
            self.write_text(
                buf,
                self.layout.pos.y,
                border_col + border_width.saturating_sub(1),
                "╯",
                self.layout.area,
                border_style,
            );
        }
        self.layout.pos.y += 1;
    }

    /// The block's highlighting, from the memo when it has it.
    pub(super) fn highlight_block(
        &mut self,
        lang_display: Option<&str>,
        code: &str,
    ) -> Option<Highlighting> {
        // Highlighting runs first, over the block, because the highlighter borrows
        // `self` for the syntax set and theme while `render_code_line` needs it
        // mutably for the layout cursor. Lines are borrowed from `code`, so this
        // costs one small `Vec` of byte ranges per line and copies no text.
        //
        // Only a *tagged* block is highlighted; an untagged one renders as plain text
        // rather than being read as some default language. The measure pass draws
        // nothing at all, so it highlights nothing.
        let syntax = lang_display
            .filter(|_| !self.layout.measuring)
            .and_then(|l| self.render_context.syntax_set().find_syntax_by_token(l));

        let theme_name = self.current_theme_name.as_deref();
        let key = memo_key(theme_name, lang_display, code);
        match self.highlight_memo.get(key) {
            hit @ Some(_) => hit,
            None => syntax.map(|syntax| {
                // A miss is either a block seen for the first time or one that grew
                // since the last frame — a tool call's contents arriving. syntect
                // carries parse state line to line, so a block cannot be coloured
                // from its middle *cold*; it can be continued from a kept state,
                // which is what makes the streaming case cost the new lines rather
                // than all of them.
                let ident = memo_key(theme_name, lang_display, "");
                let (lines, parsed) = self.highlight_memo.highlight_growing(
                    syntax,
                    self.render_context.theme(),
                    self.render_context.syntax_set(),
                    ident,
                    code,
                );
                // What the scroll-cost tests assert on (docs/tui.md): a frame that
                // hits the memo colours nothing, and a frame on a growing block
                // colours only what arrived.
                self.document.highlighted += parsed;
                self.highlight_memo.insert(key, lines)
            }),
        }
    }
}
