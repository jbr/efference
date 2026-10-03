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
use efference::tui::inline_list::{self, ListFrame, ListRow, move_key};
use efference::tui::resume_list::{COLUMNS, ResumeKey, ResumePage};
use ratatui::{buffer::Buffer, layout::Rect};
use std::io;

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
    /// The resume page, shared with the TUI's session switcher.
    resume: ResumePage,
}

impl<'a> App<'a> {
    pub(super) fn new(menu: &'a Menu, here: &Path) -> Self {
        Self {
            menu,
            here: tilde(here),
            page: Page::Top,
            top: 0,
            resume: ResumePage::new(menu.archived.clone()),
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
                    self.resume.reset();
                }
            },
            _ => {
                move_key(&mut self.top, items.len(), key);
            }
        }
        Outcome::Continue
    }

    fn resume_key(&mut self, key: KeyEvent) -> Outcome {
        match self.resume.handle(key) {
            ResumeKey::Back => self.page = Page::Top,
            ResumeKey::Pick(id) => return Outcome::Pick(Pick::Resume { id }),
            ResumeKey::Continue => {}
        }
        Outcome::Continue
    }

    pub(super) fn paste(&mut self, text: &str) {
        if self.page == Page::Resume {
            self.resume.paste(text);
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

    /// Lines the viewport needs for the taller page, within `terminal` lines.
    pub(super) fn viewport_height(&self, terminal: u16) -> u16 {
        let top = inline_list::height_for(self.top_rows().len(), false);
        let resume = self.resume.height();
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
            Page::Resume => self.resume.render(
                buf,
                area,
                styles,
                &format!("archived sessions in {}", self.here),
            ),
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
