use ratatui::{buffer::Buffer, layout::Rect, style::Modifier};

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
        let mut style = self.style(span.style);
        style = style.add_modifier(modifier);

        // Underline clickable spans to make them discoverable
        if span.action.is_some() {
            style = style.add_modifier(Modifier::UNDERLINED);
        }

        let start_col = self.layout.pos.x;
        let start_row = self.layout.pos.y;

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

        // Handle newlines in span text
        for (line_idx, line) in span.text.split('\n').enumerate() {
            if line_idx > 0 {
                self.layout.pos.y += 1;
                self.layout.pos.x = self.layout.indent;
                // Draw blockquote markers on new line
                self.draw_blockquote_markers(buf);
            }

            // Word wrap if line is too long
            let mut remaining = line;
            while !remaining.is_empty() {
                // Calculate available width: columns from current to edge (exclusive)
                // area.width is the total width, so valid columns are 0 to area.width-1
                let available_width = self.layout.area.width.saturating_sub(self.layout.pos.x);

                if available_width == 0 {
                    // No space left on this line, wrap to next
                    self.layout.pos.y += 1;
                    self.layout.pos.x = self.layout.indent;
                    // Draw blockquote markers on new line
                    self.draw_blockquote_markers(buf);
                    continue;
                }

                if display_width(remaining) <= available_width as usize {
                    // Fits on current line
                    self.write_text(
                        buf,
                        self.layout.pos.y,
                        self.layout.pos.x,
                        remaining,
                        self.layout.area,
                        style,
                    );
                    self.layout.pos.x += display_width(remaining) as u16;
                    break;
                } else {
                    // Need to wrap - find best break point
                    let truncate_at = available_width as usize;

                    // First try to find a good break point (whitespace or after punctuation)
                    let wrap_pos = find_wrap_position(remaining, truncate_at);

                    if let Some(wrap_at) = wrap_pos {
                        let (chunk, rest) = remaining.split_at(wrap_at);
                        self.write_text(
                            buf,
                            self.layout.pos.y,
                            self.layout.pos.x,
                            chunk,
                            self.layout.area,
                            style,
                        );
                        self.layout.pos.y += 1;
                        self.layout.pos.x = self.layout.indent;
                        // Draw blockquote markers on new line
                        self.draw_blockquote_markers(buf);
                        remaining = rest.trim_start(); // Skip leading whitespace on next line
                    } else {
                        // No good break point within available width
                        // Look for the next break point beyond the available width
                        // This creates ragged right margins but avoids splitting words
                        if let Some(next_space) = remaining.find(char::is_whitespace) {
                            // Check if the word will fit on the current line
                            let word_width = display_width(&remaining[..next_space]);
                            if word_width <= available_width as usize {
                                // Word fits on current line, write it
                                let (chunk, rest) = remaining.split_at(next_space);
                                self.write_text(
                                    buf,
                                    self.layout.pos.y,
                                    self.layout.pos.x,
                                    chunk,
                                    self.layout.area,
                                    style,
                                );
                                self.layout.pos.y += 1;
                                self.layout.pos.x = self.layout.indent;
                                // Draw blockquote markers on new line
                                self.draw_blockquote_markers(buf);
                                remaining = rest.trim_start();
                            } else if self.layout.pos.x == self.layout.indent {
                                // Already at the start of a fresh line and the word
                                // still overflows the full text column — it fits on
                                // no line. Hard-cut it so we make progress; otherwise
                                // pos.y increments forever and overflows (a panic).
                                let cut = hard_cut(remaining, available_width as usize);
                                let (chunk, rest) = remaining.split_at(cut);
                                self.write_text(
                                    buf,
                                    self.layout.pos.y,
                                    self.layout.pos.x,
                                    chunk,
                                    self.layout.area,
                                    style,
                                );
                                self.layout.pos.y += 1;
                                self.layout.pos.x = self.layout.indent;
                                self.draw_blockquote_markers(buf);
                                remaining = rest;
                            } else {
                                // Word doesn't fit here but may on a fresh, full-width
                                // line — wrap and retry there (next pass hard-cuts if
                                // it still doesn't fit).
                                self.layout.pos.y += 1;
                                self.layout.pos.x = self.layout.indent;
                                // Draw blockquote markers on new line
                                self.draw_blockquote_markers(buf);
                                // Don't modify remaining, continue on next line and try again
                            }
                        } else {
                            // No whitespace at all in remaining text
                            // If it fits, write it; otherwise wrap first
                            if display_width(remaining) <= available_width as usize {
                                self.write_text(
                                    buf,
                                    self.layout.pos.y,
                                    self.layout.pos.x,
                                    remaining,
                                    self.layout.area,
                                    style,
                                );
                                self.layout.pos.x += display_width(remaining) as u16;
                                break;
                            } else if self.layout.pos.x == self.layout.indent {
                                // No whitespace and already at line start: the token
                                // is wider than the whole column. Hard-cut so we
                                // advance instead of looping pos.y to overflow.
                                let cut = hard_cut(remaining, available_width as usize);
                                let (chunk, rest) = remaining.split_at(cut);
                                self.write_text(
                                    buf,
                                    self.layout.pos.y,
                                    self.layout.pos.x,
                                    chunk,
                                    self.layout.area,
                                    style,
                                );
                                self.layout.pos.y += 1;
                                self.layout.pos.x = self.layout.indent;
                                self.draw_blockquote_markers(buf);
                                remaining = rest;
                            } else {
                                // Doesn't fit here, but a fresh full-width line might
                                // hold it — wrap and retry (hard-cut next pass if not).
                                self.layout.pos.y += 1;
                                self.layout.pos.x = self.layout.indent;
                                // Draw blockquote markers on new line
                                self.draw_blockquote_markers(buf);
                                // Continue to try writing on next line
                            }
                        }
                    }
                }
            }
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
}

/// Calculate the display width of text, accounting for tabs rendered as 4 spaces
pub(super) fn display_width(text: &str) -> usize {
    text.chars().map(|ch| if ch == '\t' { 4 } else { 1 }).sum()
}

/// Block-wrap `text` into visual rows of at most `width` display columns,
/// returning the **byte offset at which each visual row begins** (the first row
/// starts at 0 implicitly, so the result holds one entry per *continuation* row;
/// an empty result means the text fits on a single row).
///
/// This is the shared wrap *policy* (DESIGN: single source of truth for where
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
        if display_width(remaining) <= width {
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

/// Largest char-aligned byte prefix of `text` whose display width fits `width`,
/// but always at least one char so wrapping makes progress on an over-wide glyph.
fn hard_cut(text: &str, width: usize) -> usize {
    let mut cols = 0;
    let mut end = 0;
    for (i, ch) in text.char_indices() {
        let w = if ch == '\t' { 4 } else { 1 };
        if cols + w > width {
            break;
        }
        cols += w;
        end = i + ch.len_utf8();
    }
    if end == 0 {
        // First glyph already exceeds width — take it whole so we advance.
        text.chars().next().map_or(text.len(), char::len_utf8)
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

    // Find the byte position that corresponds to max_width display columns (accounting for tabs)
    let mut display_cols = 0;
    let mut search_end = 0;
    for (idx, ch) in text.char_indices() {
        let char_width = if ch == '\t' { 4 } else { 1 };
        if display_cols + char_width > max_width {
            break;
        }
        display_cols += char_width;
        search_end = idx + ch.len_utf8();
    }

    if search_end == 0 {
        return None;
    }

    let search_range = &text[..search_end];

    // First priority: break at whitespace
    if let Some(pos) = search_range.rfind(char::is_whitespace) {
        // Avoid breaking if it would leave a very short word (< 3 display cols) on next line
        // This prevents orphans like "a" or "is" at the start of a line
        let remaining_width = display_width(&text[pos..]);
        if pos > 0 && remaining_width > 3 {
            return Some(pos);
        }
        // If the remaining part is short enough, it's ok to break here
        if remaining_width <= max_width / 2 {
            return Some(pos);
        }
    }

    // Second priority: break after certain punctuation (., ,, ;, :, ), ])
    // This helps with long sentences without spaces
    for (i, ch) in search_range.char_indices().rev() {
        if matches!(ch, '.' | ',' | ';' | ':' | ')' | ']' | '}') {
            // Break after the punctuation
            if i + 1 < search_range.len() {
                return Some(i + 1);
            }
        }
    }

    // Third priority: break at word boundaries (after lowercase before uppercase)
    // This helps with camelCase or PascalCase identifiers
    for i in (1..search_range.len()).rev() {
        let chars: Vec<char> = search_range.chars().collect();
        if i < chars.len() - 1 {
            let prev = chars[i - 1];
            let curr = chars[i];
            if prev.is_lowercase() && curr.is_uppercase() {
                return Some(i);
            }
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::{wrap_points, display_width};

    /// Split `text` at the byte boundaries `wrap_points` reports, for legible
    /// assertions on the visual rows.
    fn rows(text: &str, width: usize) -> Vec<String> {
        let mut out = Vec::new();
        let mut prev = 0;
        for b in wrap_points(text, width) {
            out.push(text[prev..b].to_string());
            prev = b;
        }
        out.push(text[prev..].to_string());
        out
    }

    #[test]
    fn short_text_is_one_row() {
        assert!(wrap_points("hello", 40).is_empty());
    }

    #[test]
    fn breaks_at_word_boundaries() {
        // 30 cols: the break falls on a space, never mid-word, and the breaking
        // space stays on the preceding row so the next starts clean.
        let r = rows("the quick brown fox jumps over the lazy dog", 30);
        assert!(r.len() >= 2, "wrapped into multiple rows");
        for row in &r {
            assert!(display_width(row) <= 30, "row {row:?} within width");
            assert!(!row.starts_with(' '), "no leading space on {row:?}");
        }
        // Reassembling the rows reproduces the input exactly (no dropped chars) —
        // the property the composer's cursor mapping depends on.
        assert_eq!(r.concat(), "the quick brown fox jumps over the lazy dog");
    }

    #[test]
    fn hard_breaks_a_word_longer_than_the_width() {
        // No whitespace to break on, so it cuts to make progress.
        let r = rows("superlongunbreakabletoken", 10);
        assert!(r.len() >= 3);
        assert_eq!(r.concat(), "superlongunbreakabletoken");
        assert!(r.iter().all(|row| display_width(row) <= 10));
    }

    #[test]
    fn zero_width_never_loops() {
        assert!(wrap_points("anything", 0).is_empty());
    }
}
