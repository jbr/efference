//! The model picker's content for the shared list (`tui/inline_list.rs`,
//! docs/tui.md): the chips, and the columns a model row has.
//!
//! A model row reads: the id relative to the chips, the name when it adds
//! information, up to three dim facts (context, price, age), and a trailer. At
//! narrow widths the facts go first (age, then price, then context), then the
//! name, and only then is the id truncated.

use super::render::Styles;
use super::*;
use crate::client::CatalogStatus;
use crate::tui::inline_list::{self, CellStyle, Column, ListFrame, ListInput, ListRow};
use ratatui::{buffer::Buffer, layout::Rect};

/// The model row's columns: id, name, three dim facts, a dim trailer. At narrow
/// widths the facts go first (age, then price, then context), then the name.
const COLUMNS: [Column; 6] = [
    Column::new(44, CellStyle::Primary),
    Column::new(30, CellStyle::Text).droppable(),
    Column::new(usize::MAX, CellStyle::Dim).droppable(),
    Column::new(usize::MAX, CellStyle::Dim).droppable(),
    Column::new(usize::MAX, CellStyle::Dim).droppable(),
    Column::new(usize::MAX, CellStyle::Dim).unpadded(),
];

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
pub(super) fn price_label(price: crate::inference::api::Price) -> String {
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
        let now = crate::config::history::now();
        let rows: Vec<ListRow> = self
            .picker_rows()
            .iter()
            .map(|row| ListRow {
                cells: vec![
                    row.id.clone(),
                    row.name.clone(),
                    row.context.map_or_default(context_label),
                    row.price.map_or_default(price_label),
                    row.created
                        .filter(|created| *created > 0)
                        .map_or_default(|created| age_label(created, now)),
                    row.trailer.clone(),
                ],
                dim: row.dim,
                separator: None,
            })
            .collect();
        let chips: Vec<String> = path.iter().map(|chip| chip.label().to_string()).collect();
        // What under a chip needs saying is the typed-id path, which no row shows
        // until something is typed.
        let placeholder = match path.is_empty() {
            true => "type to filter every connection's models",
            false => "type to filter, or to name any model id",
        };
        inline_list::render(
            &ListFrame {
                question: role.label(),
                input: Some(ListInput {
                    chips: &chips,
                    query,
                    placeholder,
                }),
                columns: &COLUMNS,
                rows: &rows,
                selected: *selected,
                empty: self.picker_placeholder(),
                status: &self.picker_status(),
                position_replaces_status: self.note.is_none(),
                keys: self.keys(),
            },
            buf,
            area,
            styles,
        );
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
