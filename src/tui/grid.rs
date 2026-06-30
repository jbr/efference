//! The framed character-grid encoder shared by the snapshot harness and the live
//! dev screenshot keybind (docs/tui.md).
//!
//! [`frame_grid`] turns a rendered ratatui [`Buffer`] into a bordered, ruled,
//! text-only grid: a column ruler, a row gutter, a box border, and an optional
//! caret line under the terminal cursor. Colors are intentionally dropped — only
//! cell *symbols* are captured — so the same output serves a committed `insta`
//! `.snap` (the harness) and a live screenshot dumped to disk (`Ctrl+S` in the
//! TUI). Keeping one encoder means a live capture is byte-comparable to a test
//! snapshot: if a hand-authored fixture drifts from a real reachable state, the
//! diff shows it. [`frame_grid_region`] frames just a sub-rectangle (the sidebar),
//! so a wide-terminal capture isn't ruinously verbose (`Alt+S`).

use ratatui::{buffer::Buffer, layout::Rect};
use unicode_width::UnicodeWidthStr;

/// Frame the whole buffer (columns `0..width`) as a ruled character grid; see the
/// module docs. `cursor`, if given, draws a `^` caret line under that position.
pub(crate) fn frame_grid(buf: &Buffer, width: u16, cursor: Option<(u16, u16)>) -> String {
    let rows = collect_rows(buf, Rect::new(0, 0, width, buf.area.height));
    render_framed(&rows, width as usize, cursor)
}

/// Frame only a sub-rectangle of the buffer — e.g. the docked sidebar's columns — so
/// a capture of a wide terminal stays narrow. Columns are numbered from the region's
/// left edge; no cursor caret (the slice is chrome, not the composer).
pub(crate) fn frame_grid_region(buf: &Buffer, region: Rect) -> String {
    let rows = collect_rows(buf, region);
    render_framed(&rows, region.width as usize, None)
}

/// Collect the cell symbols of `region`, one `String` per row, right-trimmed; drop
/// trailing all-blank rows.
fn collect_rows(buf: &Buffer, region: Rect) -> Vec<String> {
    let mut rows: Vec<String> = Vec::with_capacity(region.height as usize);
    for y in region.y..region.y.saturating_add(region.height).min(buf.area.height) {
        let mut s = String::new();
        for x in region.x..region.x.saturating_add(region.width).min(buf.area.width) {
            s.push_str(buf[(x, y)].symbol());
        }
        rows.push(s.trim_end().to_string());
    }
    while rows.last().is_some_and(|r| r.is_empty()) {
        rows.pop();
    }
    rows
}

/// Render collected `rows` (each ≤ `w` display columns) as a bordered, ruled grid:
///
/// ```text
///      0         1
///      0123456789012345     <- column ruler (tens row shown when width > 9)
///    ┌────────────────┐
///  0 │› hello world   │     <- row gutter | content padded to width | border
///    └────────────────┘
/// ```
///
/// When `cursor` is given, a caret line is drawn beneath its row with a `^` under the
/// exact column.
fn render_framed(rows: &[String], w: usize, cursor: Option<(u16, u16)>) -> String {
    let gutter = "   "; // two-digit row index + space
    let mut out = String::new();

    // Tens ruler (only meaningful once the width crosses a tens boundary).
    if w > 9 {
        out.push_str(gutter);
        out.push(' '); // skip the left border column
        for x in 0..w {
            out.push(if x % 10 == 0 {
                char::from_digit((x / 10 % 10) as u32, 10).unwrap()
            } else {
                ' '
            });
        }
        out.push('\n');
    }
    // Units ruler.
    out.push_str(gutter);
    out.push(' ');
    for x in 0..w {
        out.push(char::from_digit((x % 10) as u32, 10).unwrap());
    }
    out.push('\n');

    // Top border.
    out.push_str(gutter);
    out.push('┌');
    out.push_str(&"─".repeat(w));
    out.push_str("┐\n");

    // Content rows, padded to the true display width so the right border aligns
    // even across wide (CJK/emoji) glyphs. A caret line follows the cursor's row.
    for (y, row) in rows.iter().enumerate() {
        out.push_str(&format!("{y:>2} │"));
        out.push_str(row);
        let used = UnicodeWidthStr::width(row.as_str());
        if used < w {
            out.push_str(&" ".repeat(w - used));
        }
        out.push_str("│\n");

        if let Some((cx, cy)) = cursor
            && cy as usize == y
        {
            let col = (cx as usize).min(w.saturating_sub(1));
            out.push_str("   │");
            out.push_str(&" ".repeat(col));
            out.push('^');
            out.push_str(&" ".repeat(w - col - 1));
            out.push_str("│\n");
        }
    }

    // Bottom border.
    out.push_str(gutter);
    out.push('└');
    out.push_str(&"─".repeat(w));
    out.push_str("┘\n");
    out
}
