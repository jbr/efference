//! The model picker's drawing (docs/tui.md): the input line with its chips, and
//! rows laid out in columns aligned across the visible window.
//!
//! A model row reads: the id relative to the chips, the name when it adds
//! information, up to three dim facts (context, price, age), and a trailer. At
//! narrow widths the facts go first (age, then price, then context), then the
//! name, and only then is the id truncated.

use super::picker_rows::PickerRow;
use super::render::Styles;
use super::*;
use crate::client::CatalogStatus;
use ratatui::{
    buffer::Buffer,
    layout::{Constraint, Layout, Rect},
    style::Modifier,
    text::{Line as Text, Span},
    widgets::{Paragraph, Widget},
};

/// The widest an id or a name column grows before its text is cut with `…`.
const ID_CAP: usize = 44;
const NAME_CAP: usize = 30;

/// Gutter before every row: the cursor mark and a space.
const GUTTER: usize = 2;

/// `1M`, `272k`, `8192`: a context window in tokens.
pub(super) fn context_label(tokens: u64) -> String {
    match tokens {
        1_000_000.. => {
            let millions = tokens as f64 / 1_000_000.0;
            match (millions - millions.round()).abs() < 0.05 {
                true => format!("{}M", millions.round()),
                false => format!("{millions:.1}M"),
            }
        }
        1_000.. => format!("{}k", (tokens + 500) / 1_000),
        _ => tokens.to_string(),
    }
}

/// `$0.15/$0.50`: USD per million input and output tokens.
pub(super) fn price_label(price: crate::api::Price) -> String {
    if price.input == 0.0 && price.output == 0.0 {
        return "free".to_string();
    }
    let one = |usd: f64| match usd > 0.0 && usd < 0.01 {
        true => format!("${usd:.3}"),
        false => format!("${usd:.2}"),
    };
    format!("{}/{}", one(price.input), one(price.output))
}

/// `3d`, `5w`, `8mo`, `2y`: how long ago a model was created.
pub(super) fn age_label(created: i64, now: i64) -> String {
    let days = (now - created).max(0) / 86_400;
    match days {
        0..7 => format!("{days}d"),
        7..60 => format!("{}w", days / 7),
        60..730 => format!("{}mo", days / 30),
        _ => format!("{}y", days / 365),
    }
}

/// Cut `text` to `width` characters, marking the cut.
fn fit(text: &str, width: usize) -> String {
    match text.chars().count() > width {
        false => text.to_string(),
        true => match width {
            0 => String::new(),
            _ => text.chars().take(width - 1).chain(['…']).collect(),
        },
    }
}

/// The columns one window of rows is laid out in.
struct Columns {
    id: usize,
    name: usize,
    /// Widths of the fact columns still shown, in order; `0` marks a column no
    /// visible row has a value for.
    facts: Vec<usize>,
    trailer: usize,
}

impl Columns {
    fn total(&self) -> usize {
        let shown = |width: &usize| (*width > 0).then_some(2 + width);
        self.id
            + [self.name, self.trailer]
                .iter()
                .chain(&self.facts)
                .filter_map(shown)
                .sum::<usize>()
    }

    /// Widths that fit `available`, dropping what matters least first.
    fn fitted(rows: &[[String; 6]], available: usize) -> Self {
        let widest = |column: usize| {
            rows.iter()
                .map(|row| row[column].chars().count())
                .max()
                .unwrap_or(0)
        };
        let mut columns = Columns {
            id: widest(0).min(ID_CAP),
            name: widest(1).min(NAME_CAP),
            facts: vec![widest(2), widest(3), widest(4)],
            trailer: widest(5),
        };
        while columns.total() > available {
            if columns.facts.pop().is_some() {
                continue;
            }
            if columns.name > 0 {
                columns.name = 0;
                continue;
            }
            columns.id = available.saturating_sub(columns.total() - columns.id);
            break;
        }
        columns
    }
}

impl Form {
    pub(super) fn render_picker(&self, buf: &mut Buffer, area: Rect, styles: Styles) {
        let Page::Picker {
            role,
            path,
            query,
            selected,
            ..
        } = &self.page
        else {
            return;
        };
        let rows = self.picker_rows();
        let selected = (*selected).min(rows.len().saturating_sub(1));
        let capacity = usize::from(area.height.saturating_sub(4)).max(1);
        let shown = rows.len().min(capacity).max(1);
        let layout = Layout::vertical([
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(u16::try_from(shown).unwrap_or(u16::MAX)),
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Min(0),
        ])
        .split(area);
        Paragraph::new(Text::from(vec![
            Span::styled("? ", styles.selected.add_modifier(Modifier::BOLD)),
            Span::styled(role.label(), styles.title.add_modifier(Modifier::BOLD)),
        ]))
        .render(layout[0], buf);

        let mut input = vec![Span::raw("  ")];
        for chip in path {
            input.push(Span::styled(format!("[{}]", chip.label()), styles.selected));
            input.push(Span::raw(" "));
        }
        input.push(Span::styled(query.with_cursor(), styles.text));
        // A bare caret does not read as a text field, so an empty query says
        // what typing does. The chips to its left already name the scope; what
        // under one needs saying is the typed-id path, which no row shows
        // until something is typed.
        if query.text().is_empty() {
            let placeholder = match path.is_empty() {
                true => "type to filter every connection's models",
                false => "type to filter, or to name any model id",
            };
            input.push(Span::styled(
                placeholder,
                styles.hint.add_modifier(Modifier::ITALIC),
            ));
        }
        Paragraph::new(Text::from(input)).render(layout[1], buf);

        let first = (selected + 1).saturating_sub(capacity);
        let window = &rows[first.min(rows.len())..rows.len().min(first + capacity)];
        let lines: Vec<Text> = match window.is_empty() {
            true => vec![Text::styled(
                format!("  {}", self.picker_placeholder()),
                styles.hint,
            )],
            false => self.picker_lines(window, first, selected, usize::from(area.width), styles),
        };
        Paragraph::new(lines).render(layout[2], buf);

        let scrolled = rows.len() > capacity && self.note.is_none();
        let status = match scrolled {
            true => format!("{}/{}", selected + 1, rows.len()),
            false => self.picker_status(),
        };
        Paragraph::new(format!("  {status}"))
            .style(styles.hint)
            .render(layout[3], buf);
        Paragraph::new(format!("  {}", self.keys()))
            .style(styles.hint)
            .render(layout[4], buf);
    }

    /// The rows of one window, columns aligned among them.
    fn picker_lines(
        &self,
        window: &[PickerRow],
        first: usize,
        selected: usize,
        width: usize,
        styles: Styles,
    ) -> Vec<Text<'static>> {
        let now = crate::history::now();
        let cells: Vec<[String; 6]> = window
            .iter()
            .map(|row| {
                [
                    row.id.clone(),
                    row.name.clone(),
                    row.context.map(context_label).unwrap_or_default(),
                    row.price.map(price_label).unwrap_or_default(),
                    row.created
                        .filter(|created| *created > 0)
                        .map(|created| age_label(created, now))
                        .unwrap_or_default(),
                    row.trailer.clone(),
                ]
            })
            .collect();
        let columns = Columns::fitted(&cells, width.saturating_sub(GUTTER));
        window
            .iter()
            .zip(&cells)
            .enumerate()
            .map(|(offset, (row, cell))| {
                let index = first + offset;
                let id_style = match (row.dim, index == selected) {
                    (true, _) => styles.hint,
                    (false, true) => styles.selected,
                    (false, false) => styles.text,
                };
                let mut spans = vec![
                    Span::styled(if index == selected { "❯ " } else { "  " }, styles.selected),
                    Span::styled(
                        format!("{:w$}", fit(&cell[0], columns.id), w = columns.id),
                        id_style,
                    ),
                ];
                if columns.name > 0 {
                    spans.push(Span::styled(
                        format!("  {:w$}", fit(&cell[1], columns.name), w = columns.name),
                        styles.text,
                    ));
                }
                for (slot, width) in columns.facts.iter().enumerate() {
                    if *width > 0 {
                        spans.push(Span::styled(
                            format!("  {:w$}", cell[2 + slot], w = *width),
                            styles.hint,
                        ));
                    }
                }
                if columns.trailer > 0 && !row.trailer.is_empty() {
                    spans.push(Span::styled(format!("  {}", row.trailer), styles.hint));
                }
                Text::from(spans)
            })
            .collect()
    }

    /// What the window says when it has no rows.
    fn picker_placeholder(&self) -> &'static str {
        match &self.page {
            Page::Picker { path, query, .. } if !path.is_empty() && query.text().is_empty() => {
                "no models listed — type a model id"
            }
            _ if self.connections().is_empty() => "no connections read from this host",
            _ => "nothing matches",
        }
    }

    /// The line under the rows: the picker's own state first — a connection that
    /// is being read, or one that failed — then what the form itself has to say.
    pub(super) fn picker_status(&self) -> String {
        if let Some(note) = &self.note {
            return note.clone();
        }
        if let Page::Picker { path, .. } = &self.page
            && let Some(Chip::Connection(connection)) = path.first()
            && let Some(entry) = self.catalog_entry(connection)
        {
            match entry.status {
                CatalogStatus::Missing | CatalogStatus::Stale if entry.models.is_empty() => {
                    return format!("reading {connection}'s models…");
                }
                CatalogStatus::Failed | CatalogStatus::Unavailable => {
                    return format!(
                        "{connection}: {}",
                        entry.error.as_deref().unwrap_or("its model list failed")
                    );
                }
                _ => {}
            }
        }
        self.status()
    }
}
