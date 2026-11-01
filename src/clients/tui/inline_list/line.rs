//! The single-line editor behind the inline prompts' typed pages (docs/tui.md).
//!
//! The composer's editing binds, restricted to one line: the same motion, kill,
//! and undo keys work here as in the transcript's composer, because they are the
//! composer's own. What a line does not take is the composer's submit and
//! newline keys — the page decides what Enter means, and there is nowhere for a
//! second line to go.

use crate::clients::tui::composer::{Composer, ComposerAction};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

#[derive(Debug)]
pub struct Line(Composer);

impl Line {
    pub fn new(text: &str) -> Self {
        let mut composer = Composer::new();
        composer.set_text(text);
        Self(composer)
    }

    pub fn text(&self) -> String {
        self.0.text()
    }

    /// The text with a cursor mark at the caret, for drawing.
    pub fn with_cursor(&self) -> String {
        let text = self.0.text();
        let (before, after) = text.split_at(self.0.cursor());
        format!("{before}▏{after}")
    }

    /// Feed an editing key. Enter, C-j, Alt-Enter and Tab are the caller's or
    /// nobody's; everything else is the composer's binding table.
    pub fn handle_key(&mut self, key: KeyEvent) {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Enter | KeyCode::Tab | KeyCode::BackTab => {}
            KeyCode::Char('j') if ctrl => {}
            // Up and C-p at the top edge would spill out of the composer; the
            // line has no edge to spill over, and a one-line buffer's vertical
            // motion is a no-op.
            _ => {
                let _: ComposerAction = self.0.handle_key(key);
            }
        }
    }

    /// Insert pasted text on one line: control characters (newlines included)
    /// are dropped, as the form has always done.
    pub fn paste(&mut self, text: &str) {
        let text: String = text.chars().filter(|c| !c.is_control()).collect();
        self.0.paste_text(&text);
    }
}

impl Clone for Line {
    fn clone(&self) -> Self {
        Self::new(&self.0.text())
    }
}

impl PartialEq for Line {
    fn eq(&self, other: &Self) -> bool {
        self.0.text() == other.0.text()
    }
}

impl Eq for Line {}
