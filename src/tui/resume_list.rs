//! The resume page: a filterable list of archived sessions (docs/tui.md).
//!
//! One implementation for the two places that offer to revive a dormant
//! session: the bare-`efference` picker's inline page and the session
//! switcher's modal. It is drawn with the shared inline list
//! (`tui/inline_list.rs`), so both look and key the same. [`ResumePage`] is pure
//! state — rows, a query, a cursor, a note — with keys in, a [`ResumeKey`] out,
//! and a buffer drawn; the container does the reading of the rows and the
//! `PATCH {"state": "active"}` that a pick asks for (docs/control-api.md).
//!
//! Every row has one shape: the title (the id when there is none), the id
//! dimmed, then the age dimmed. The query keeps the rows whose title or id holds
//! every word of it. Esc clears a non-empty query first and otherwise asks to go
//! back, as in the model picker.

use crate::protocol::inventory::InventoryRow;
use crate::tui::inline::Styles;
use crate::tui::inline_list::{
    self, CellStyle, Column, Line, ListFrame, ListInput, ListKey, ListRow, list_key,
};
use crossterm::event::{KeyCode, KeyEvent, KeyEventKind};
use ratatui::{buffer::Buffer, layout::Rect};

/// Title, id, then age. The title has no cap of its own: it is what a person
/// reads a row by, so it takes the width the terminal has and is cut only when
/// that runs out. The picker's top page uses the same shape for its live rows.
pub const COLUMNS: [Column; 3] = [
    Column::new(usize::MAX, CellStyle::Primary),
    Column::new(40, CellStyle::Dim),
    Column::new(usize::MAX, CellStyle::Dim)
        .droppable()
        .unpadded(),
];

/// One dormant session the page offers to resume.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResumeRow {
    pub id: String,
    pub title: Option<String>,
    /// How long ago it was last active, as the row shows it.
    pub age: Option<String>,
}

impl ResumeRow {
    /// The rows for inventory rows, in the order given (most recently active
    /// first, as the scan and the route return them). A row with a `problem` is
    /// not offered: it could not be resumed.
    pub fn from_inventory(rows: Vec<InventoryRow>, now_ms: u64) -> Vec<Self> {
        rows.into_iter()
            .filter(|row| row.problem.is_none())
            .map(|row| Self {
                age: row.last_active_at.map(|at| age(now_ms, at)),
                id: row.id,
                title: row.title,
            })
            .collect()
    }
}

/// `then_ms` as time before `now_ms`, to the largest whole unit: "3h ago".
pub fn age(now_ms: u64, then_ms: u64) -> String {
    let seconds = now_ms.saturating_sub(then_ms) / 1000;
    match seconds {
        0..60 => "just now".to_string(),
        60..3600 => format!("{}m ago", seconds / 60),
        3600..86_400 => format!("{}h ago", seconds / 3600),
        _ => format!("{}d ago", seconds / 86_400),
    }
}

/// Milliseconds since the Unix epoch, for [`ResumeRow::from_inventory`].
pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| since.as_millis() as u64)
}

/// What a key asked of the container.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResumeKey {
    Continue,
    /// Revive this session.
    Pick(String),
    /// Esc on an empty query: leave the page.
    Back,
}

/// The page's state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResumePage {
    rows: Vec<ResumeRow>,
    query: Line,
    selected: usize,
    /// The rows are still being read.
    loading: bool,
    /// The id being revived, until the host answers.
    resuming: Option<String>,
    /// A line in the status slot: what the page waits on, or what went wrong.
    note: Option<String>,
}

impl ResumePage {
    pub fn new(rows: Vec<ResumeRow>) -> Self {
        Self {
            rows,
            query: Line::new(""),
            selected: 0,
            loading: false,
            resuming: None,
            note: None,
        }
    }

    /// A page whose rows have not landed yet.
    pub fn loading() -> Self {
        Self {
            loading: true,
            note: Some("reading the archived sessions…".into()),
            ..Self::new(Vec::new())
        }
    }

    /// The rows landed.
    pub fn set_rows(&mut self, rows: Vec<ResumeRow>) {
        self.rows = rows;
        self.loading = false;
        self.note = None;
        self.selected = 0;
    }

    /// The rows could not be read; `note` says why.
    pub fn read_failed(&mut self, note: String) {
        self.loading = false;
        self.note = Some(note);
    }

    /// A pick is out with the host. Enter does nothing until it answers.
    pub fn begin_resume(&mut self, id: &str) {
        self.resuming = Some(id.to_string());
        self.note = Some(format!("resuming {id}…"));
    }

    /// The host refused or failed; the page stays, with the host's message.
    pub fn resume_failed(&mut self, note: String) {
        self.resuming = None;
        self.note = Some(note);
    }

    pub fn query(&self) -> String {
        self.query.text()
    }

    /// The rows whose title or id holds every word of the query.
    pub fn matches(&self) -> Vec<&ResumeRow> {
        let query = self.query.text().to_lowercase();
        self.rows
            .iter()
            .filter(|session| {
                let haystack = format!(
                    "{} {}",
                    session.title.as_deref().unwrap_or_default(),
                    session.id
                )
                .to_lowercase();
                query.split_whitespace().all(|word| haystack.contains(word))
            })
            .collect()
    }

    pub fn handle(&mut self, key: KeyEvent) -> ResumeKey {
        if key.kind == KeyEventKind::Release {
            return ResumeKey::Continue;
        }
        if key.code == KeyCode::Esc {
            if self.query.text().is_empty() {
                return ResumeKey::Back;
            }
            self.query = Line::new("");
            self.selected = 0;
            return ResumeKey::Continue;
        }
        let len = self.matches().len();
        let pressed = list_key(&mut self.query, &mut self.selected, len, key);
        if pressed == ListKey::Enter
            && self.resuming.is_none()
            && let Some(session) = self.matches().get(self.selected)
        {
            return ResumeKey::Pick(session.id.clone());
        }
        ResumeKey::Continue
    }

    pub fn paste(&mut self, text: &str) {
        self.query.paste(text);
        self.selected = 0;
    }

    /// Back to a fresh query on the first row, as when the page is entered.
    pub fn reset(&mut self) {
        self.query = Line::new("");
        self.selected = 0;
    }

    /// Lines a viewport needs to show every row, query line included.
    pub fn height(&self) -> usize {
        match self.rows.is_empty() {
            true => 0,
            false => inline_list::height_for(self.rows.len(), true),
        }
    }

    fn list_rows(&self) -> Vec<ListRow> {
        self.matches()
            .into_iter()
            .map(|session| {
                let title = session.title.as_deref().unwrap_or(&session.id);
                ListRow::new(vec![
                    title.into(),
                    session.id.clone(),
                    session.age.clone().unwrap_or_default(),
                ])
            })
            .collect()
    }

    /// Draw the page into `area` under `question`.
    pub fn render(&self, buf: &mut Buffer, area: Rect, styles: Styles, question: &str) {
        let rows = self.list_rows();
        let status = match (&self.note, self.query.text().trim().is_empty()) {
            (Some(note), _) => note.clone(),
            (None, true) => String::new(),
            (None, false) => format!("{} of {}", rows.len(), self.rows.len()),
        };
        let empty = match (self.loading, self.rows.is_empty()) {
            (true, _) => "",
            (false, true) => "no archived sessions here",
            (false, false) => "nothing matches",
        };
        inline_list::render(
            &ListFrame {
                question,
                input: Some(ListInput {
                    chips: &[],
                    query: &self.query,
                    placeholder: "type to filter by title or id",
                }),
                columns: &COLUMNS,
                rows: &rows,
                selected: self.selected,
                empty,
                status: &status,
                position_replaces_status: self.note.is_none(),
                keys: "↑↓ move · enter resumes · esc goes back",
            },
            buf,
            area,
            styles,
        );
    }
}

#[cfg(test)]
mod test;
