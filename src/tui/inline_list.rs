//! The list the inline prompts share (docs/tui.md): the model picker in the
//! new-session form and the bare-`efference` session picker.
//!
//! One frame: a `?`-marked question, an optional input line (chips, then a query
//! that filters as it is typed), a window of rows in columns aligned across the
//! rows visible in it, a status line, and a key hint. The selected row is marked
//! with a `❯` and coloured rather than highlighted across its width; trailing
//! columns are dim. Rows are one line each and never wrap: a column is cut with
//! `…` at its cap, and when the width runs short whole columns drop (the later
//! droppable ones first) before the first column is cut.
//!
//! The keys every list handles — `Up`/`Down`, `C-p`/`C-n`, and query editing —
//! are [`list_key`]; what Enter, Tab and Esc mean is the page's.

mod line;

pub use line::Line;

use super::session_form::render::Styles;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{
    buffer::Buffer,
    layout::{Constraint, Layout, Rect},
    style::Modifier,
    text::{Line as Text, Span},
    widgets::{Paragraph, Widget},
};

/// Gutter before every row: the cursor mark and a space.
const GUTTER: usize = 2;

/// How a column's cells are drawn.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CellStyle {
    /// The row's own colour: the accent when selected, dim when the row is.
    Primary,
    Text,
    Dim,
}

/// One column's layout rule. The first column is the row's label: it is never
/// dropped and is the last to be cut.
#[derive(Clone, Copy, Debug)]
pub struct Column {
    /// The widest the column grows before its text is cut with `…`.
    pub cap: usize,
    /// May be left out entirely when the width runs short.
    pub droppable: bool,
    /// Padded to the column's width. The last column is left unpadded so its
    /// rows carry no trailing blanks.
    pub padded: bool,
    pub style: CellStyle,
}

impl Column {
    pub const fn new(cap: usize, style: CellStyle) -> Self {
        Self {
            cap,
            droppable: false,
            padded: true,
            style,
        }
    }

    pub const fn droppable(mut self) -> Self {
        self.droppable = true;
        self
    }

    pub const fn unpadded(mut self) -> Self {
        self.padded = false;
        self
    }
}

/// One row: a cell per column, or a separator line that cannot be selected.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ListRow {
    pub cells: Vec<String>,
    /// The first column is drawn dim, for a row that is shown but unavailable.
    pub dim: bool,
    /// Text of a separator; the cells are ignored.
    pub separator: Option<String>,
}

impl ListRow {
    pub fn new(cells: Vec<String>) -> Self {
        Self {
            cells,
            ..Self::default()
        }
    }

    pub fn separator(text: impl Into<String>) -> Self {
        Self {
            separator: Some(text.into()),
            ..Self::default()
        }
    }
}

/// Everything one frame of the list shows.
pub struct ListFrame<'a> {
    pub question: &'a str,
    /// The input line; `None` for a list that takes no query.
    pub input: Option<ListInput<'a>>,
    pub columns: &'a [Column],
    pub rows: &'a [ListRow],
    /// Index among the selectable rows (separators are not counted).
    pub selected: usize,
    /// What the window says when there are no rows.
    pub empty: &'a str,
    pub status: &'a str,
    /// A scrolled window reports `n/m` in place of `status`.
    pub position_replaces_status: bool,
    pub keys: &'a str,
}

/// The input line's content.
pub struct ListInput<'a> {
    pub chips: &'a [String],
    pub query: &'a Line,
    /// Shown, in italics, while the query is empty.
    pub placeholder: &'a str,
}

/// How many rows the window shows in `height` lines, given the chrome around it.
pub fn capacity(height: u16, has_input: bool) -> usize {
    usize::from(height.saturating_sub(3 + u16::from(has_input))).max(1)
}

/// The most viewport lines a list of `rows` display rows needs.
pub fn height_for(rows: usize, has_input: bool) -> usize {
    rows.max(1) + 3 + usize::from(has_input)
}

/// What a key did to a list.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ListKey {
    /// The cursor moved.
    Moved,
    /// The query changed; the cursor is back on the first row.
    Edited,
    Enter,
    /// Not a list key, or one that changed nothing.
    Ignored,
}

/// The cursor keys: `Up`/`Down` and `C-p`/`C-n`. `len` is the number of
/// selectable rows. True when `key` was one of them.
pub fn move_key(selected: &mut usize, len: usize, key: KeyEvent) -> bool {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let last = len.saturating_sub(1);
    *selected = (*selected).min(last);
    match key.code {
        KeyCode::Up => *selected = selected.saturating_sub(1),
        KeyCode::Down => *selected = (*selected + 1).min(last),
        KeyCode::Char('p') if ctrl => *selected = selected.saturating_sub(1),
        KeyCode::Char('n') if ctrl => *selected = (*selected + 1).min(last),
        _ => return false,
    }
    true
}

/// The keys every list with a query handles: the cursor keys, Enter reported
/// to the page, and everything else as query editing.
pub fn list_key(query: &mut Line, selected: &mut usize, len: usize, key: KeyEvent) -> ListKey {
    if move_key(selected, len, key) {
        return ListKey::Moved;
    }
    if key.code == KeyCode::Enter {
        return ListKey::Enter;
    }
    let before = query.text();
    query.handle_key(key);
    if query.text() == before {
        return ListKey::Ignored;
    }
    *selected = 0;
    ListKey::Edited
}

/// Cut `text` to `width` characters, marking the cut.
pub fn fit(text: &str, width: usize) -> String {
    match text.chars().count() > width {
        false => text.to_string(),
        true => match width {
            0 => String::new(),
            _ => text.chars().take(width - 1).chain(['…']).collect(),
        },
    }
}

/// Column widths for one window of rows; `0` is a column left out.
struct Widths(Vec<usize>);

impl Widths {
    fn total(&self) -> usize {
        self.0[0]
            + self.0[1..]
                .iter()
                .filter(|width| **width > 0)
                .map(|width| 2 + width)
                .sum::<usize>()
    }

    /// Widths that fit `available`, dropping what matters least first.
    fn fitted(columns: &[Column], cells: &[&[String]], available: usize) -> Self {
        let mut widths = Self(
            columns
                .iter()
                .enumerate()
                .map(|(at, column)| {
                    cells
                        .iter()
                        .map(|row| row.get(at).map_or(0, |cell| cell.chars().count()))
                        .max()
                        .unwrap_or(0)
                        .min(column.cap)
                })
                .collect(),
        );
        while widths.total() > available {
            let droppable = (1..columns.len())
                .rev()
                .find(|at| columns[*at].droppable && widths.0[*at] > 0);
            match droppable {
                Some(at) => widths.0[at] = 0,
                None => {
                    widths.0[0] = available.saturating_sub(widths.total() - widths.0[0]);
                    break;
                }
            }
        }
        widths
    }
}

/// Draw one frame of the list into `area`.
pub fn render(frame: &ListFrame, buf: &mut Buffer, area: Rect, styles: Styles) {
    let has_input = frame.input.is_some();
    let selectable = frame.rows.iter().filter(|r| r.separator.is_none()).count();
    let selected = frame.selected.min(selectable.saturating_sub(1));
    // Where the selected row sits among all display rows, separators included.
    let position = frame
        .rows
        .iter()
        .enumerate()
        .filter(|(_, row)| row.separator.is_none())
        .nth(selected)
        .map_or(0, |(at, _)| at);
    let capacity = capacity(area.height, has_input);
    let shown = frame.rows.len().min(capacity).max(1);
    let mut constraints = vec![Constraint::Length(1)];
    if has_input {
        constraints.push(Constraint::Length(1));
    }
    constraints.extend([
        Constraint::Length(u16::try_from(shown).unwrap_or(u16::MAX)),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Min(0),
    ]);
    let layout = Layout::vertical(constraints).split(area);
    let mut slot = layout.iter().copied();
    let mut next = || slot.next().unwrap_or_default();

    Paragraph::new(Text::from(vec![
        Span::styled("? ", styles.selected.add_modifier(Modifier::BOLD)),
        Span::styled(frame.question, styles.title.add_modifier(Modifier::BOLD)),
    ]))
    .render(next(), buf);

    if let Some(input) = &frame.input {
        let mut spans = vec![Span::raw("  ")];
        for chip in input.chips {
            spans.push(Span::styled(format!("[{chip}]"), styles.selected));
            spans.push(Span::raw(" "));
        }
        spans.push(Span::styled(input.query.with_cursor(), styles.text));
        // A bare caret does not read as a text field, so an empty query says what
        // typing does.
        if input.query.text().is_empty() && !input.placeholder.is_empty() {
            spans.push(Span::styled(
                input.placeholder,
                styles.hint.add_modifier(Modifier::ITALIC),
            ));
        }
        Paragraph::new(Text::from(spans)).render(next(), buf);
    }

    let first = (position + 1).saturating_sub(capacity);
    let window = &frame.rows[first.min(frame.rows.len())..frame.rows.len().min(first + capacity)];
    let lines = match window.is_empty() {
        true => vec![Text::styled(format!("  {}", frame.empty), styles.hint)],
        false => row_lines(
            frame,
            window,
            first,
            position,
            usize::from(area.width),
            styles,
        ),
    };
    Paragraph::new(lines).render(next(), buf);

    let scrolled = frame.rows.len() > capacity && frame.position_replaces_status;
    let status = match scrolled {
        true => format!("{}/{}", selected + 1, selectable),
        false => frame.status.to_string(),
    };
    Paragraph::new(format!("  {status}"))
        .style(styles.hint)
        .render(next(), buf);
    Paragraph::new(format!("  {}", frame.keys))
        .style(styles.hint)
        .render(next(), buf);
}

/// The rows of one window, columns aligned among them.
fn row_lines(
    frame: &ListFrame,
    window: &[ListRow],
    first: usize,
    position: usize,
    width: usize,
    styles: Styles,
) -> Vec<Text<'static>> {
    let cells: Vec<&[String]> = window
        .iter()
        .filter(|row| row.separator.is_none())
        .map(|row| row.cells.as_slice())
        .collect();
    let widths = Widths::fitted(frame.columns, &cells, width.saturating_sub(GUTTER));
    window
        .iter()
        .enumerate()
        .map(|(offset, row)| {
            if let Some(text) = &row.separator {
                return Text::styled(format!("  ── {text}"), styles.hint);
            }
            let chosen = first + offset == position;
            let mut spans = vec![Span::styled(
                if chosen { "❯ " } else { "  " },
                styles.selected,
            )];
            for (at, column) in frame.columns.iter().enumerate() {
                let w = widths.0[at];
                let cell = row.cells.get(at).map_or("", String::as_str);
                if w == 0 || (at > 0 && !column.padded && cell.is_empty()) {
                    continue;
                }
                // A dimmed cell is a hint whatever its column style, a plain one text,
                // and only a primary cell in the chosen row takes the selection.
                let style = match (column.style, row.dim, chosen) {
                    (CellStyle::Dim, ..) | (CellStyle::Primary, true, _) => styles.hint,
                    (CellStyle::Text, ..) | (CellStyle::Primary, false, false) => styles.text,
                    (CellStyle::Primary, false, true) => styles.selected,
                };
                let lead = if at == 0 { "" } else { "  " };
                let text = match column.padded {
                    true => format!("{lead}{:w$}", fit(cell, w)),
                    false => format!("{lead}{}", fit(cell, w)),
                };
                spans.push(Span::styled(text, style));
            }
            Text::from(spans)
        })
        .collect()
}
