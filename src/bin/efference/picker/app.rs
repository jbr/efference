//! The picker's two pages as an inline ratatui app (docs/operating.md,
//! docs/tui.md).
//!
//! Drawn with the list and viewport the new-session form uses
//! (`tui/inline_list.rs`, `tui/inline.rs`), so the two look and key the same
//! when one follows the other. The *top* page is the create entry, a separator
//! and a row per live session here, and the entry that opens the *resume* page;
//! the resume page lists every resumable archived session under a query line
//! that filters on title and id. Every row has one shape: the title (the id
//! when there is none), the id dimmed, then the status or age dimmed.
//!
//! The viewport is sized once, for the taller page, bounded by the terminal; the
//! window scrolls within it. [`App`] is pure state — keys in, an [`Outcome`] out,
//! a buffer drawn — and [`ask`] is the terminal loop around it.

use super::*;
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use efference::tui::inline::{Inline, Styles, terminal_rows};
use efference::tui::inline_list::{
    self, CellStyle, Column, Line, ListFrame, ListInput, ListKey, ListRow, list_key, move_key,
};
use ratatui::{buffer::Buffer, layout::Rect};
use std::io;

/// Title, id, then status or age.
const COLUMNS: [Column; 3] = [
    Column::new(60, CellStyle::Primary),
    Column::new(40, CellStyle::Dim),
    Column::new(usize::MAX, CellStyle::Dim)
        .droppable()
        .unpadded(),
];

/// The most lines the picker's viewport takes, however many sessions there are.
const MAX_HEIGHT: u16 = 16;

const CREATE: &str = "create a new session";
const RESUME: &str = "resume an archived session…";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Page {
    Top,
    Resume,
}

/// What a key asked of the loop.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Outcome {
    Continue,
    Pick(Pick),
    Cancel,
}

/// A selectable row on the top page.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Item {
    Create,
    Live(usize),
    Resume,
}

pub(super) struct App<'a> {
    menu: &'a Menu,
    here: String,
    page: Page,
    /// The cursor on the top page, among its selectable rows.
    top: usize,
    query: Line,
    /// The cursor on the resume page, among the rows the query leaves.
    selected: usize,
}

impl<'a> App<'a> {
    pub(super) fn new(menu: &'a Menu, here: &Path) -> Self {
        Self {
            menu,
            here: tilde(here),
            page: Page::Top,
            top: 0,
            query: Line::new(""),
            selected: 0,
        }
    }

    fn items(&self) -> Vec<Item> {
        let mut items = vec![Item::Create];
        items.extend((0..self.menu.rows.len()).map(Item::Live));
        if !self.menu.archived.is_empty() {
            items.push(Item::Resume);
        }
        items
    }

    /// The archived sessions whose title or id holds every word of the query.
    fn matches(&self) -> Vec<&Archived> {
        let query = self.query.text().to_lowercase();
        self.menu
            .archived
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

    pub(super) fn handle(&mut self, key: KeyEvent) -> Outcome {
        if key.kind == KeyEventKind::Release {
            return Outcome::Continue;
        }
        if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
            return Outcome::Cancel;
        }
        match self.page {
            Page::Top => self.top_key(key),
            Page::Resume => self.resume_key(key),
        }
    }

    fn top_key(&mut self, key: KeyEvent) -> Outcome {
        let items = self.items();
        match key.code {
            KeyCode::Esc => return Outcome::Cancel,
            KeyCode::Enter => match items.get(self.top).copied().unwrap_or(Item::Create) {
                Item::Create => return Outcome::Pick(Pick::Create),
                Item::Live(at) => {
                    let row = &self.menu.rows[at];
                    return Outcome::Pick(Pick::Attach {
                        host: row.host.clone(),
                        id: row.id.clone(),
                    });
                }
                Item::Resume => {
                    self.page = Page::Resume;
                    self.query = Line::new("");
                    self.selected = 0;
                }
            },
            _ => {
                move_key(&mut self.top, items.len(), key);
            }
        }
        Outcome::Continue
    }

    fn resume_key(&mut self, key: KeyEvent) -> Outcome {
        // As in the model picker (docs/tui.md): Esc clears a filter first, and
        // only an empty one leaves the page.
        if key.code == KeyCode::Esc {
            if self.query.text().is_empty() {
                self.page = Page::Top;
            } else {
                self.query = Line::new("");
                self.selected = 0;
            }
            return Outcome::Continue;
        }
        let len = self.matches().len();
        if list_key(&mut self.query, &mut self.selected, len, key) == ListKey::Enter
            && let Some(session) = self.matches().get(self.selected)
        {
            return Outcome::Pick(Pick::Resume {
                id: session.id.clone(),
            });
        }
        Outcome::Continue
    }

    pub(super) fn paste(&mut self, text: &str) {
        if self.page == Page::Resume {
            self.query.paste(text);
            self.selected = 0;
        }
    }

    fn top_rows(&self) -> Vec<ListRow> {
        let mut rows = vec![ListRow::new(vec![CREATE.into()])];
        if !self.menu.rows.is_empty() {
            rows.push(ListRow::separator("running here"));
            rows.extend(self.menu.rows.iter().map(|row| {
                let title = row.title.as_deref().unwrap_or(&row.id);
                ListRow::new(vec![title.into(), row.id.clone(), row.status.clone()])
            }));
        }
        if !self.menu.archived.is_empty() {
            rows.push(ListRow::new(vec![
                RESUME.into(),
                String::new(),
                self.menu.archived.len().to_string(),
            ]));
        }
        rows
    }

    fn resume_rows(&self) -> Vec<ListRow> {
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

    /// Lines the viewport needs for the taller page, within `terminal` lines.
    pub(super) fn viewport_height(&self, terminal: u16) -> u16 {
        let top = inline_list::height_for(self.top_rows().len(), false);
        let resume = match self.menu.archived.is_empty() {
            true => 0,
            false => inline_list::height_for(self.menu.archived.len(), true),
        };
        let bound = usize::from(MAX_HEIGHT.min(terminal.saturating_sub(1)).max(5));
        u16::try_from(top.max(resume).min(bound)).unwrap_or(MAX_HEIGHT)
    }

    pub(super) fn render(&self, buf: &mut Buffer, area: Rect, styles: Styles) {
        buf.set_style(area, styles.text);
        match self.page {
            Page::Top => {
                let rows = self.top_rows();
                // The cursor's index among selectable rows is the item's.
                inline_list::render(
                    &ListFrame {
                        question: &format!("sessions in {}", self.here),
                        input: None,
                        columns: &COLUMNS,
                        rows: &rows,
                        selected: self.top,
                        empty: "",
                        status: "",
                        position_replaces_status: true,
                        keys: "↑↓ move · enter picks · esc cancels",
                    },
                    buf,
                    area,
                    styles,
                );
            }
            Page::Resume => {
                let rows = self.resume_rows();
                let status = match self.query.text().trim().is_empty() {
                    true => String::new(),
                    false => format!("{} of {}", rows.len(), self.menu.archived.len()),
                };
                inline_list::render(
                    &ListFrame {
                        question: &format!("archived sessions in {}", self.here),
                        input: Some(ListInput {
                            chips: &[],
                            query: &self.query,
                            placeholder: "type to filter by title or id",
                        }),
                        columns: &COLUMNS,
                        rows: &rows,
                        selected: self.selected,
                        empty: "nothing matches",
                        status: &status,
                        position_replaces_status: true,
                        keys: "↑↓ move · enter resumes · esc goes back",
                    },
                    buf,
                    area,
                    styles,
                );
            }
        }
    }
}

/// The line that points at sessions the hosts hold elsewhere.
pub(super) fn elsewhere_note(count: usize) -> Option<String> {
    let (many, is, it) = match count {
        0 => return None,
        1 => ("", "is", "it"),
        _ => ("s", "are", "them"),
    };
    Some(format!(
        "{count} session{many} on this host {is} in other directories; run `efference` there to see {it}."
    ))
}

/// Ask. `None` is a cancel. With nothing here to attach to or resume, the answer
/// is `Create` without a question: the one entry would say nothing, and the form
/// that follows has its own cancel.
pub(super) fn ask(menu: &Menu, here: &Path, styles: Styles) -> io::Result<Option<Pick>> {
    if let Some(note) = elsewhere_note(menu.elsewhere) {
        println!("{note}");
    }
    if menu.rows.is_empty() && menu.archived.is_empty() {
        return Ok(Some(Pick::Create));
    }
    let mut app = App::new(menu, here);
    let mut viewport = Inline::open(app.viewport_height(terminal_rows()))?;
    let outcome = (|| {
        loop {
            viewport.draw(|buf, area| app.render(buf, area, styles))?;
            let outcome = match event::read()? {
                Event::Key(key) => app.handle(key),
                Event::Paste(text) => {
                    app.paste(&text);
                    Outcome::Continue
                }
                _ => Outcome::Continue,
            };
            match outcome {
                Outcome::Continue => {}
                Outcome::Pick(pick) => break Ok::<_, io::Error>(Some(pick)),
                Outcome::Cancel => break Ok(None),
            }
        }
    })();
    // Erased before the form or the TUI draws: the next viewport opens where
    // this one began.
    viewport.close()?;
    outcome
}

#[cfg(test)]
mod test;
