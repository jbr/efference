use ratatui::{
    buffer::Buffer,
    style::{Color, Style},
};
use syntect::easy::HighlightLines;
use syntect::util::LinesWithEndings;

use super::render_span::{display_width, wrap_points};
use super::state::InteractiveState;

/// One styled run within a source line, as a `(start, end, style)` byte range —
/// a highlighter token, or the whole line when there is no highlighting. A wrapped
/// row clips each of these to the slice of the line it shows.
type Segment = (usize, usize, Style);

/// The box a code block draws itself inside — the columns every row of content has
/// to agree on. Passed as one value so the per-line render can't drift from the
/// borders the top and bottom rules were drawn at.
struct BoxFrame {
    border_col: u16,
    content_col: u16,
    border_width: u16,
    border_style: Style,
    /// Columns available between the borders — what content wraps to.
    inner_width: usize,
}

// Code block borders are outdented to the left of content so that the code text
// aligns with surrounding prose, and the border is purely decorative.
const CODE_BLOCK_BORDER_WIDTH: u16 = 2; // "│ " takes 2 columns
const CODE_BLOCK_BORDER_OUTDENT: i16 = -2; // Draw border 2 columns left of content

impl<'a> InteractiveState<'a> {
    /// Render a code block, syntax-highlighted and labelled **only when the
    /// language is known**. An untagged block (`lang: None`) is plain text with a
    /// bare border: no highlighting, no `❬…❭` tag.
    ///
    /// It used to default to `rust`, which was correct in ferritin — fenced blocks
    /// in rustdoc are Rust unless they say otherwise — and wrong here, where the
    /// commonest untagged block is a tool result. `ls` output was being labelled
    /// `❬rust❭` and highlighted as Rust.
    pub(super) fn render_code_block(&mut self, lang: Option<&str>, code: &str, buf: &mut Buffer) {
        // The rustdoc fence attributes still mean Rust wherever they appear, which
        // includes model-written markdown about Rust.
        let lang_display = match lang {
            Some(
                "no_run" | "should_panic" | "ignore" | "compile_fail" | "edition2015"
                | "edition2018" | "edition2021" | "edition2024",
            ) => Some("rust"),
            other => other,
        };

        // Border is outdented (to the left of content) so code text aligns with surrounding text
        let border_col = self
            .layout
            .indent
            .saturating_add_signed(CODE_BLOCK_BORDER_OUTDENT);
        let content_col = self.layout.indent; // Code content stays at indent

        // Calculate code block dimensions accounting for content position
        let available_width = self.layout.area.width.saturating_sub(content_col);
        // Display width, not byte length: a line of box-drawing or accented text is
        // three bytes a column and would size the border two columns too wide each.
        // `display_width` is the renderer's one column oracle — `write_text` writes a
        // cell per char (a tab as four), so anything else would disagree with what
        // actually lands in the buffer.
        let max_line_width = code
            .lines()
            .map(display_width)
            .max()
            .unwrap_or(0)
            .min((available_width.saturating_sub(4)) as usize); // Leave room for border and padding

        // Account for language label in border width: ╭───❬rust❭─╮. An untagged
        // block has no label, so the border is sized by its content alone.
        let lang_label = lang_display.map(|l| format!("❬{l}❭"));
        // Count actual display width (number of grapheme clusters, not bytes)
        let label_display_width = lang_label.as_ref().map_or(0, |l| l.chars().count());
        let min_border_for_label = match &lang_label {
            Some(_) => label_display_width as u16 + 6, // label + some padding
            None => 0,
        };
        let border_width = ((max_line_width + 4).max(min_border_for_label as usize))
            .min(available_width as usize) as u16;

        let border_style = self.theme.code_block_border_style;

        // Top border with language label: ╭─────❬rust❭─╮
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

        // Columns available *inside* the box: the border eats "│ " on the left and
        // " │" on the right. Content wraps to this rather than running past the
        // border, which is what used to clip the tail of every long line.
        let inner_width = border_width.saturating_sub(4).max(1) as usize;
        let frame = BoxFrame {
            border_col,
            content_col,
            border_width,
            border_style,
            inner_width,
        };

        // Highlighting runs first, over the whole block, because the highlighter
        // borrows `self` for the syntax set and theme while `render_code_line` needs
        // it mutably for the layout cursor. Lines are borrowed from `code`, so this
        // costs one small `Vec` of byte ranges per line and copies no text.
        //
        // Only a *tagged* block is highlighted; an untagged one is one plain segment
        // per line rather than being read as some default language.
        let lines: Vec<(&str, Vec<Segment>)> = match lang_display
            .and_then(|l| self.render_context.syntax_set().find_syntax_by_token(l))
        {
            Some(syntax) => {
                let mut highlighter = HighlightLines::new(syntax, self.render_context.theme());
                LinesWithEndings::from(code)
                    .map(|line| {
                        // syntect needs the line ending to keep its state machine
                        // right, but the newline is never drawn.
                        let drawn = line.trim_end_matches('\n');
                        let segments = match highlighter
                            .highlight_line(line, self.render_context.syntax_set())
                        {
                            // Byte offsets into `drawn`, so a wrapped row can clip
                            // each token to the slice of the line it actually shows.
                            Ok(ranges) => {
                                let mut segments = Vec::with_capacity(ranges.len());
                                let mut at = 0;
                                for (style, text) in ranges {
                                    let end = (at + text.len()).min(drawn.len());
                                    if at < end {
                                        let fg = style.foreground;
                                        segments.push((
                                            at,
                                            end,
                                            Style::default().fg(Color::Rgb(fg.r, fg.g, fg.b)),
                                        ));
                                    }
                                    at += text.len();
                                }
                                segments
                            }
                            Err(_) => vec![(0, drawn.len(), Style::default())],
                        };
                        (drawn, segments)
                    })
                    .collect()
            }
            None => code
                .lines()
                .map(|line| (line, vec![(0, line.len(), Style::default())]))
                .collect(),
        };

        // Render code content with side borders (no background color).
        for (line, segments) in &lines {
            self.render_code_line(buf, line, segments, &frame);
        }

        // Bottom border: ╰─────╯
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

    /// Draw one source line inside the box, wrapped to the frame's inner width, as
    /// however many visual rows that takes. `segments` are [`Segment`]s over `line`.
    ///
    /// Wrapping goes through [`wrap_points`], the renderer's shared break policy, so
    /// a code block breaks where prose and the composer break: at whitespace when it
    /// can, at punctuation or a camelCase seam when it can't, and hard-cut only for a
    /// run wider than the box. Row boundaries partition the line with no gaps, so
    /// clipping each token to a row loses nothing.
    fn render_code_line(
        &mut self,
        buf: &mut Buffer,
        line: &str,
        segments: &[Segment],
        frame: &BoxFrame,
    ) {
        let mut row_starts = vec![0usize];
        row_starts.extend(wrap_points(line, frame.inner_width));

        for (i, &row_start) in row_starts.iter().enumerate() {
            let row_end = row_starts.get(i + 1).copied().unwrap_or(line.len());
            if self.row_visible(self.layout.pos.y) {
                // Left border and padding
                self.write_text(
                    buf,
                    self.layout.pos.y,
                    frame.border_col,
                    "│ ",
                    self.layout.area,
                    frame.border_style,
                );

                // Each token, clipped to the slice of the line this row shows.
                let mut col = frame.content_col;
                for &(seg_start, seg_end, style) in segments {
                    let start = seg_start.max(row_start);
                    let end = seg_end.min(row_end);
                    if start >= end {
                        continue;
                    }
                    let text = &line[start..end];
                    self.write_text(buf, self.layout.pos.y, col, text, self.layout.area, style);
                    // Advance by the columns just written, not the bytes: the
                    // highlighter splits a line into tokens, so a multi-byte one used
                    // to push every later token right (`├── anyhow v1.      0.86`).
                    col += display_width(text) as u16;
                }

                // Right border and padding
                self.write_text(
                    buf,
                    self.layout.pos.y,
                    frame.border_col + frame.border_width.saturating_sub(2),
                    " │",
                    self.layout.area,
                    frame.border_style,
                );
            }
            self.layout.pos.y += 1;
        }
    }
}
