#[cfg(test)]
mod test;

use std::collections::VecDeque;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::rc::Rc;

use ratatui::{
    buffer::Buffer,
    style::{Color, Style},
};
use syntect::util::LinesWithEndings;

use super::render_span::{display_width, wrap_points};
use super::state::InteractiveState;

/// One styled run within a source line, as a `(start, end, style)` byte range —
/// a highlighter token, or the whole line when there is no highlighting. A wrapped
/// row clips each of these to the slice of the line it shows.
type Segment = (usize, usize, Style);

/// A block's highlighting, one `Vec<Segment>` per source line.
type Highlighting = Rc<Vec<Vec<Segment>>>;

/// How many segments the memo holds before it starts evicting. Roughly 5 MB at
/// ~24 bytes a segment; a 2000-line source file runs about 30k.
const MEMO_SEGMENT_CAP: usize = 200_000;

/// Computed highlighting, kept between frames (docs/tui.md — what a frame costs).
///
/// A *visible* top-level node is re-laid-out on every frame, and a multi-screen code
/// block is one node — so without this, syntect re-parses the whole block ~33 times
/// a second while the operator scrolls past it, and parse state has to be carried
/// from the block's first line every time, so the cost is worst exactly where the
/// operator has scrolled furthest. Colours are a pure function of (theme, language,
/// text), so they are computed once and kept.
///
/// Keyed by a hash of those three inputs, so a theme change or an edited block
/// misses and recomputes rather than showing stale colour. Bounded by
/// [`MEMO_SEGMENT_CAP`], evicting oldest-first.
#[derive(Debug, Default)]
pub(super) struct HighlightMemo {
    entries: VecDeque<(u64, Highlighting)>,
    segments: usize,
    /// One slot, because only one block grows at a time: the live tool call's.
    growing: Option<GrowingHighlight>,
}

/// Highlighting for a block that is still being **appended to** — a tool call's
/// contents arriving mid-stream (docs/tui.md).
///
/// [`HighlightMemo`] is keyed on the block's text, so a growing block misses it every
/// frame and re-parses from line 1. The text is append-only, and syntect exposes its
/// per-line state (`HighlightLines::state` / `from_state`), so only the lines that
/// arrived since the last frame need parsing.
///
/// The saved state is the state after the last **complete** line. A block's final line
/// is usually still arriving and can mis-lex (an unterminated string literal colours
/// the rest of the line as a string), so it is recomputed each frame from the saved
/// state and never committed to it.
#[derive(Debug)]
struct GrowingHighlight {
    /// Identity of what is being highlighted — theme and language, not text. A
    /// theme switch or a different block resets rather than continuing.
    ident: u64,
    /// The prefix already highlighted: complete lines only. Held as text so the
    /// continuation check is an exact `starts_with` rather than a hash that could
    /// collide into showing one file's colours over another's.
    prefix: String,
    /// Segments for the lines in `prefix`, one entry per line.
    lines: Vec<Vec<Segment>>,
    /// syntect's per-line state as of the end of `prefix`.
    parse: syntect::parsing::ParseState,
    highlight: syntect::highlighting::HighlightState,
}

impl HighlightMemo {
    fn get(&self, key: u64) -> Option<Highlighting> {
        self.entries
            .iter()
            .find(|(k, _)| *k == key)
            .map(|(_, v)| Rc::clone(v))
    }

    fn insert(&mut self, key: u64, lines: Vec<Vec<Segment>>) -> Highlighting {
        let count: usize = lines.iter().map(Vec::len).sum();
        let value: Highlighting = Rc::new(lines);
        self.entries.push_back((key, Rc::clone(&value)));
        self.segments += count;
        while self.segments > MEMO_SEGMENT_CAP && self.entries.len() > 1 {
            if let Some((_, evicted)) = self.entries.pop_front() {
                self.segments = self
                    .segments
                    .saturating_sub(evicted.iter().map(Vec::len).sum::<usize>());
            }
        }
        value
    }

    /// Highlight `code`, reusing whatever of it was highlighted last frame.
    ///
    /// Returns the per-line segments and **how many lines were actually parsed** —
    /// the number `document.highlighted` reports and the render-cost tests assert
    /// on. For a block that grew by a line since the last frame that is one or two,
    /// not the whole block.
    ///
    /// Falls back to parsing everything whenever `code` is not a continuation of
    /// what was highlighted before (a different block, a theme change, an edit
    /// rather than an append), so correctness never depends on the guess being
    /// right.
    fn highlight_growing(
        &mut self,
        syntax: &syntect::parsing::SyntaxReference,
        theme: &syntect::highlighting::Theme,
        syntax_set: &syntect::parsing::SyntaxSet,
        ident: u64,
        code: &str,
    ) -> (Vec<Vec<Segment>>, usize) {
        use syntect::easy::HighlightLines;

        // Continue only from an exact prefix of this very text; anything else
        // starts over.
        let resume = self
            .growing
            .take()
            .filter(|g| g.ident == ident && !g.prefix.is_empty() && code.starts_with(&g.prefix));

        let (mut lines, from, mut highlighter) = match resume {
            Some(g) => {
                let at = g.prefix.len();
                let h = HighlightLines::from_state(theme, g.highlight, g.parse);
                (g.lines, at, h)
            }
            None => (Vec::new(), 0, HighlightLines::new(syntax, theme)),
        };

        // Complete lines extend the saved state; the trailing partial line does not.
        let tail = &code[from..];
        let complete_upto = tail.rfind('\n').map_or(0, |i| i + 1);
        let mut parsed = 0;
        for line in LinesWithEndings::from(&tail[..complete_upto]) {
            lines.push(line_segments(&mut highlighter, line, syntax_set));
            parsed += 1;
        }

        // Save the state *here*, before the partial line, then carry on past it.
        let (highlight, parse) = highlighter.state();
        self.growing = Some(GrowingHighlight {
            ident,
            prefix: code[..from + complete_upto].to_string(),
            lines: lines.clone(),
            parse: parse.clone(),
            highlight: highlight.clone(),
        });

        let remainder = &tail[complete_upto..];
        if !remainder.is_empty() {
            let mut highlighter = HighlightLines::from_state(theme, highlight, parse);
            lines.push(line_segments(&mut highlighter, remainder, syntax_set));
            parsed += 1;
        }

        (lines, parsed)
    }
}

/// One line's syntect ranges as [`Segment`]s: byte offsets into the line *without*
/// its newline, so a wrapped row can clip each token to the slice it shows. syntect
/// needs the newline to keep its state machine right, but it is never drawn.
fn line_segments(
    highlighter: &mut syntect::easy::HighlightLines<'_>,
    line: &str,
    syntax_set: &syntect::parsing::SyntaxSet,
) -> Vec<Segment> {
    let drawn = line.trim_end_matches('\n');
    match highlighter.highlight_line(line, syntax_set) {
        Ok(ranges) => {
            let mut segments = Vec::with_capacity(ranges.len());
            let mut at = 0;
            for (style, text) in ranges {
                let end = (at + text.len()).min(drawn.len());
                if at < end {
                    let fg = style.foreground;
                    segments.push((at, end, Style::default().fg(Color::Rgb(fg.r, fg.g, fg.b))));
                }
                at += text.len();
            }
            segments
        }
        Err(_) => vec![(0, drawn.len(), Style::default())],
    }
}

/// The memo key: what the colours are a function of.
fn memo_key(theme: Option<&str>, lang: Option<&str>, code: &str) -> u64 {
    let mut hasher = DefaultHasher::new();
    theme.hash(&mut hasher);
    lang.hash(&mut hasher);
    code.hash(&mut hasher);
    hasher.finish()
}

/// The box a code block draws itself inside — the columns every row of content has
/// to agree on. Passed as one value so the per-line render can't drift from the
/// borders the top and bottom rules were drawn at.
struct BoxFrame {
    border_col: u16,
    content_col: u16,
    border_width: u16,
    border_style: Style,
    /// Columns available between the borders for *content* — what it wraps to,
    /// already less whatever the gutter takes.
    inner_width: usize,
    /// Columns reserved left of the content for line numbers; 0 when unnumbered.
    gutter_width: usize,
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
    /// Defaulting to `rust` is correct in ferritin — a fenced block in rustdoc is
    /// Rust unless it says otherwise — and wrong here, where the commonest untagged
    /// block is a tool result: `ls` output labelled `❬rust❭` and coloured as Rust.
    pub(super) fn render_code_block(&mut self, lang: Option<&str>, code: &str, buf: &mut Buffer) {
        self.render_code_frame(lang, code, None, buf);
    }

    /// The same box, with each line's **original number** in a dimmed gutter — the
    /// TUI's form for a triaged result (docs/tui.md). `numbers` is parallel to
    /// `code`'s lines; a gap in it is a gap in the source, stated by the jump rather
    /// than by a `@@` header.
    pub(super) fn render_numbered_code(
        &mut self,
        lang: Option<&str>,
        lines: &[(u32, std::borrow::Cow<'a, str>)],
        buf: &mut Buffer,
    ) {
        let code = lines
            .iter()
            .map(|(_, text)| text.as_ref())
            .collect::<Vec<_>>()
            .join("\n");
        let numbers: Vec<u32> = lines.iter().map(|(n, _)| *n).collect();
        self.render_code_frame(lang, &code, Some(&numbers), buf);
    }

    /// The shared box render. `numbers`, when present, is one original line number
    /// per line of `code`, drawn in a dimmed right-aligned gutter that continuation
    /// rows leave blank — so a wrapped line reads as one line.
    fn render_code_frame(
        &mut self,
        lang: Option<&str>,
        code: &str,
        numbers: Option<&[u32]>,
        buf: &mut Buffer,
    ) {
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

        // The gutter is as wide as the widest number it has to show, plus a space.
        let gutter_width = numbers
            .and_then(|ns| ns.iter().max())
            .map_or(0, |n| n.to_string().len() + 1);

        // Calculate code block dimensions accounting for content position
        let available_width = self.layout.area.width.saturating_sub(content_col);
        // Display width, not byte length: a line of box-drawing or accented text is
        // three bytes a column and would size the border two columns too wide each.
        // `display_width` is the renderer's one column oracle — `write_text` writes a
        // cell per char (a tab as four), so anything else would disagree with what
        // actually lands in the buffer.
        let max_line_width = code
            .lines()
            .map(|line| display_width(line) + gutter_width)
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
        // border, where the tail of a long line would be clipped.
        let inner_width = (border_width as usize)
            .saturating_sub(4 + gutter_width)
            .max(1);
        let frame = BoxFrame {
            border_col,
            content_col,
            border_width,
            border_style,
            inner_width,
            gutter_width,
        };

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
        let highlighting: Option<Highlighting> = match self.highlight_memo.get(key) {
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
        };

        // Render code content with side borders (no background color).
        for (i, line) in code.lines().enumerate() {
            let plain = [(0, line.len(), Style::default())];
            let segments = highlighting
                .as_ref()
                .and_then(|h| h.get(i))
                .map_or(&plain[..], Vec::as_slice);
            let number = numbers.and_then(|ns| ns.get(i)).copied();
            self.render_code_line(buf, line, segments, number, &frame);
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
        number: Option<u32>,
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

                // The line number, on the line's *first* row only — a continuation
                // row leaves the gutter blank, so a wrapped line still reads as one
                // line rather than as an unnumbered neighbour.
                if frame.gutter_width > 0 {
                    let gutter = match number.filter(|_| i == 0) {
                        Some(n) => format!("{n:>width$} ", width = frame.gutter_width - 1),
                        None => " ".repeat(frame.gutter_width),
                    };
                    self.write_text(
                        buf,
                        self.layout.pos.y,
                        frame.content_col,
                        &gutter,
                        self.layout.area,
                        self.theme.muted_style,
                    );
                }

                // Each token, clipped to the slice of the line this row shows.
                let mut col = frame.content_col + frame.gutter_width as u16;
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
