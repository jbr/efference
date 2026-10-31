use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Modifier, Style},
};

use super::state::InteractiveState;
use crate::tui::document::Span;

impl<'a> InteractiveState<'a> {
    /// Render a span with optional action tracking
    pub(super) fn render_span(&mut self, span: &Span<'a>, buf: &mut Buffer) {
        self.render_span_with_modifier(span, Modifier::empty(), buf);
    }

    /// Render a span with additional style modifier
    pub(super) fn render_span_with_modifier(
        &mut self,
        span: &Span<'a>,
        modifier: Modifier,
        buf: &mut Buffer,
    ) {
        let style = self.span_style(span, modifier);

        let start_col = self.layout.pos.x;
        let start_row = self.layout.pos.y;

        // Handle newlines in span text
        for (line_idx, line) in span.text.split('\n').enumerate() {
            if line_idx > 0 {
                self.next_row(buf);
            }
            self.write_wrapped(line, style, buf);
        }

        // Track action if present
        if let Some(action) = &span.action {
            // Calculate width handling wrapping (pos.x might be less than start_col if we wrapped)
            let width = if self.layout.pos.y > start_row {
                // Multi-line span - use full width of first line as clickable area
                self.layout.area.width.saturating_sub(start_col).max(1)
            } else {
                // Single line - use actual span width
                self.layout.pos.x.saturating_sub(start_col).max(1)
            };

            let rect = Rect::new(
                start_col,
                start_row,
                width,
                (self.layout.pos.y - start_row + 1).max(1),
            );
            self.render_cache.actions.push((rect, action.clone()));
        }
    }

    /// The style a span draws in at the cursor: its own, the extra modifier, and — for a
    /// clickable span — an underline, reversed while the mouse or the keyboard is on it.
    fn span_style(&self, span: &Span<'a>, modifier: Modifier) -> Style {
        let mut style = self.style(span.style);
        style = style.add_modifier(modifier);

        // Underline clickable spans to make them discoverable
        if span.action.is_some() {
            style = style.add_modifier(Modifier::UNDERLINED);
        }

        // Determine if this span should be highlighted (by mouse or keyboard)
        let should_highlight = if span.action.is_some() {
            // Check mouse hover
            let mouse_hover = self.viewport.cursor_pos.map_or_else(
                || false,
                |cursor| {
                    cursor.y == self.layout.pos.y
                        && cursor.x >= self.layout.pos.x
                        && cursor.x < self.layout.pos.x + display_width(&span.text) as u16
                },
            );

            // Check keyboard focus - this action is about to be pushed, so its index will be actions.len()
            let keyboard_focus = match self.viewport.keyboard_cursor {
                super::state::KeyboardCursor::Focused { action_index } => {
                    action_index == self.render_cache.actions.len()
                }
                _ => false,
            };

            mouse_hover || keyboard_focus
        } else {
            false
        };

        // If hovered or focused, invert colors
        if should_highlight {
            style = style.add_modifier(Modifier::REVERSED);
        }
        style
    }

    /// Move to the start of the next row, inside the blockquote markers if there are any.
    fn next_row(&mut self, buf: &mut Buffer) {
        self.layout.pos.y += 1;
        self.layout.pos.x = self.layout.indent;
        // Draw blockquote markers on new line
        self.draw_blockquote_markers(buf);
    }

    /// Write `text` at the cursor, clipped to the area.
    fn write_here(&mut self, buf: &mut Buffer, text: &str, style: Style) {
        self.write_text(
            buf,
            self.layout.pos.y,
            self.layout.pos.x,
            text,
            self.layout.area,
            style,
        );
    }

    /// Write the first `at` bytes of `text` here, start the next row, and return the rest.
    fn write_to_row_end<'t>(
        &mut self,
        text: &'t str,
        at: usize,
        style: Style,
        buf: &mut Buffer,
    ) -> &'t str {
        let (chunk, rest) = text.split_at(at);
        self.write_here(buf, chunk, style);
        self.next_row(buf);
        rest
    }

    /// Write one line of a span from the cursor, wrapping at the area's edge: at a good break
    /// point when one fits, else after the next whitespace, else hard-cut when the word is
    /// wider than the whole column.
    fn write_wrapped(&mut self, line: &str, style: Style, buf: &mut Buffer) {
        // Word wrap if line is too long
        let mut remaining = line;
        while !remaining.is_empty() {
            // Calculate available width: columns from current to edge (exclusive)
            // area.width is the total width, so valid columns are 0 to area.width-1
            let available_width = self.layout.area.width.saturating_sub(self.layout.pos.x);

            if available_width == 0 {
                // No space left on this line, wrap to next
                self.next_row(buf);
                continue;
            }

            // Bounded (see `width_upto`): this loop runs once per row, so
            // measuring the whole remainder each time makes drawing one long
            // line quadratic in its length — the same defect `wrap_points` had.
            if width_upto(remaining, available_width as usize) <= available_width as usize {
                // Fits on current line
                self.write_here(buf, remaining, style);
                self.layout.pos.x += display_width(remaining) as u16;
                break;
            }
            // Need to wrap - find best break point
            let truncate_at = available_width as usize;

            // First try to find a good break point (whitespace or after punctuation)
            let wrap_pos = find_wrap_position(remaining, truncate_at);

            if let Some(wrap_at) = wrap_pos {
                // Skip leading whitespace on next line
                remaining = self
                    .write_to_row_end(remaining, wrap_at, style, buf)
                    .trim_start();
            } else if let Some(next_space) = remaining.find(char::is_whitespace) {
                // No good break point within available width
                // Look for the next break point beyond the available width
                // This creates ragged right margins but avoids splitting words
                // Check if the word will fit on the current line
                let word_width = display_width(&remaining[..next_space]);
                if word_width <= available_width as usize {
                    // Word fits on current line, write it
                    remaining = self
                        .write_to_row_end(remaining, next_space, style, buf)
                        .trim_start();
                } else if self.layout.pos.x == self.layout.indent {
                    // Already at the start of a fresh line and the word
                    // still overflows the full text column — it fits on
                    // no line. Hard-cut it so we make progress; otherwise
                    // pos.y increments forever and overflows (a panic).
                    let cut = hard_cut(remaining, available_width as usize);
                    remaining = self.write_to_row_end(remaining, cut, style, buf);
                } else {
                    // Word doesn't fit here but may on a fresh, full-width
                    // line — wrap and retry there (next pass hard-cuts if
                    // it still doesn't fit).
                    // Don't modify remaining, continue on next line and try again
                    self.next_row(buf);
                }
            } else {
                // No whitespace at all in remaining text
                // If it fits, write it; otherwise wrap first. Bounded
                // (see `width_upto`): this is the no-whitespace path, so
                // an unbroken run reaches it once per row it occupies.
                if width_upto(remaining, available_width as usize) <= available_width as usize {
                    self.write_here(buf, remaining, style);
                    self.layout.pos.x += display_width(remaining) as u16;
                    break;
                } else if self.layout.pos.x == self.layout.indent {
                    // No whitespace and already at line start: the token
                    // is wider than the whole column. Hard-cut so we
                    // advance instead of looping pos.y to overflow.
                    let cut = hard_cut(remaining, available_width as usize);
                    remaining = self.write_to_row_end(remaining, cut, style, buf);
                } else {
                    // Doesn't fit here, but a fresh full-width line might
                    // hold it — wrap and retry (hard-cut next pass if not).
                    // Continue to try writing on next line
                    self.next_row(buf);
                }
            }
        }
    }
}

/// Calculate the display width of text, accounting for tabs rendered as 4 spaces.
/// Extended grapheme clusters are measured as a unit so layout never assigns
/// independent cells to the scalars composing an emoji or combining sequence.
pub(super) fn display_width(text: &str) -> usize {
    use unicode_segmentation::UnicodeSegmentation;
    use unicode_width::UnicodeWidthStr;

    text.graphemes(true)
        .map(|grapheme| {
            if grapheme == "\t" {
                4
            } else {
                grapheme.width()
            }
        })
        .sum()
}

#[cfg(test)]
std::thread_local! {
    static WIDTH_UPTO_GRAPHEMES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Returns and resets the graphemes inspected by [`width_upto`] on this test thread.
#[cfg(test)]
pub(super) fn take_width_upto_graphemes() -> usize {
    WIDTH_UPTO_GRAPHEMES.with(|count| count.replace(0))
}

/// [`display_width`], measured no further than `cap` columns: the exact width when
/// it is at most `cap`, and `cap + 1` (meaning "wider than `cap`") as soon as it is
/// not.
///
/// Every use of a width in the wrap policy is a *comparison* against something no
/// larger than the row width, so measuring past that point is work whose result is
/// discarded. Measuring it anyway made wrapping quadratic in the length of a line:
/// [`wrap_points`] tests the remaining text once per row it emits, so one 16k-column
/// line — a tool call's JSON input, a minified file, a base64 blob — cost
/// hundreds of milliseconds per frame, and the frames repeat while it streams.
fn width_upto(text: &str, cap: usize) -> usize {
    use unicode_segmentation::UnicodeSegmentation;
    use unicode_width::UnicodeWidthStr;

    let mut total = 0;
    for grapheme in text.graphemes(true) {
        #[cfg(test)]
        WIDTH_UPTO_GRAPHEMES.with(|count| count.set(count.get() + 1));

        total += if grapheme == "\t" {
            4
        } else {
            grapheme.width()
        };
        if total > cap {
            return cap + 1;
        }
    }
    total
}

/// Block-wrap `text` into visual rows of at most `width` display columns,
/// returning the **byte offset at which each visual row begins** (the first row
/// starts at 0 implicitly, so the result holds one entry per *continuation* row;
/// an empty result means the text fits on a single row).
///
/// This is the shared wrap *policy* (single source of truth for where
/// lines break). The document renderer's inline-flow loop and the composer's
/// block layout both lean on the same [`find_wrap_position`] oracle; the
/// composer additionally walks these boundaries to size its input area and place
/// the cursor. A breaking space is kept on the *preceding* row (the boundary
/// lands after it), so the returned offsets partition the text with no gaps —
/// which is what lets the composer map a caret to a screen cell unambiguously.
pub(super) fn wrap_points(text: &str, width: usize) -> Vec<usize> {
    let mut breaks = Vec::new();
    if width == 0 || text.is_empty() {
        return breaks;
    }
    let mut base = 0;
    while base < text.len() {
        let remaining = &text[base..];
        // Bounded: this only ever asks "does the rest fit on one row?", so it must
        // not measure a rest that is many rows long (see `width_upto`).
        if width_upto(remaining, width) <= width {
            break;
        }
        // Where to cut, as a byte offset within `remaining`.
        let cut = match find_wrap_position(remaining, width) {
            // A good break (whitespace / punctuation / camelCase) within width.
            Some(pos) if pos > 0 => pos,
            // No break within width: prefer the next whitespace beyond it (ragged
            // right, never splits a word); else a hard char-aligned cut so an
            // over-wide run still makes progress.
            _ => remaining
                .find(char::is_whitespace)
                .filter(|&p| p > 0)
                .unwrap_or_else(|| hard_cut(remaining, width)),
        };
        // Keep a breaking space on this row: advance past it so the next row
        // starts on a non-space. Other break kinds leave the char on the next row.
        let consumed = if remaining[cut..].starts_with(char::is_whitespace) {
            cut + remaining[cut..].chars().next().map_or(0, char::len_utf8)
        } else {
            cut
        };
        base += consumed.max(1);
        breaks.push(base);
    }
    // A trailing boundary exactly at end-of-text means the last cut consumed the
    // whole remainder; drop it so we don't emit an empty final row.
    if breaks.last() == Some(&text.len()) {
        breaks.pop();
    }
    breaks
}

/// Largest grapheme-aligned byte prefix of `text` whose display width fits
/// `width`, but always at least one cluster so wrapping makes progress on an
/// over-wide glyph.
fn hard_cut(text: &str, width: usize) -> usize {
    use unicode_segmentation::UnicodeSegmentation;
    use unicode_width::UnicodeWidthStr;

    let mut cols = 0;
    let mut end = 0;
    for (i, grapheme) in text.grapheme_indices(true) {
        let w = if grapheme == "\t" {
            4
        } else {
            grapheme.width()
        };
        if cols + w > width {
            break;
        }
        cols += w;
        end = i + grapheme.len();
    }
    if end == 0 {
        // First cluster already exceeds width — take it whole so we advance.
        text.graphemes(true).next().map_or(text.len(), str::len)
    } else {
        end
    }
}

/// Find the best position to wrap text within a given width
/// Returns the position after which to break, or None if no good break point exists
pub(super) fn find_wrap_position(text: &str, max_width: usize) -> Option<usize> {
    if max_width == 0 || text.is_empty() {
        return None;
    }

    // Find the byte position that corresponds to max_width display columns.
    let mut display_cols = 0;
    let mut search_end = 0;
    for (idx, grapheme) in unicode_segmentation::UnicodeSegmentation::grapheme_indices(text, true) {
        let width = if grapheme == "\t" {
            4
        } else {
            unicode_width::UnicodeWidthStr::width(grapheme)
        };
        if display_cols + width > max_width {
            break;
        }
        display_cols += width;
        search_end = idx + grapheme.len();
    }

    if search_end == 0 {
        return None;
    }

    let search_range = &text[..search_end];

    // First priority: break at whitespace.
    if let Some((pos, _)) =
        unicode_segmentation::UnicodeSegmentation::grapheme_indices(search_range, true)
            .rev()
            .find(|(_, grapheme)| grapheme.chars().all(char::is_whitespace))
    {
        // Avoid breaking if it would leave a very short word (< 3 display cols) on next line.
        // This prevents orphans like "a" or "is" at the start of a line.
        //
        // Bounded at the row width: both tests below compare against something no
        // larger than that, so the exact width of a long tail is never wanted.
        let remaining_width = width_upto(&text[pos..], max_width.max(4));
        if pos > 0 && remaining_width > 3 {
            return Some(pos);
        }
        // If the remaining part is short enough, it's ok to break here.
        if remaining_width <= max_width / 2 {
            return Some(pos);
        }
    }

    // Second priority: break after certain punctuation (., ,, ;, :, ), ]).
    // This helps with long sentences without spaces.
    for (i, grapheme) in
        unicode_segmentation::UnicodeSegmentation::grapheme_indices(search_range, true).rev()
    {
        if grapheme
            .chars()
            .last()
            .is_some_and(|ch| matches!(ch, '.' | ',' | ';' | ':' | ')' | ']' | '}'))
        {
            let end = i + grapheme.len();
            if end < search_range.len() {
                return Some(end);
            }
        }
    }

    // Third priority: break at word boundaries (after lowercase before uppercase)
    // This helps with camelCase or PascalCase identifiers.
    let graphemes: Vec<_> =
        unicode_segmentation::UnicodeSegmentation::grapheme_indices(search_range, true).collect();
    for pair in graphemes.windows(2).rev() {
        let (_, prev) = pair[0];
        let (curr_offset, curr) = pair[1];
        if prev.chars().last().is_some_and(char::is_lowercase)
            && curr.chars().next().is_some_and(char::is_uppercase)
        {
            return Some(curr_offset);
        }
    }

    None
}

#[cfg(test)]
mod test;
