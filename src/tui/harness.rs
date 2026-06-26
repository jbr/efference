//! "Playwright for TUIs" — a test harness for snapshotting the interactive TUI
//! as a plain-text character grid (no colors, no pixels).
//!
//! The render path ([`InteractiveState::render_frame`](super::state::InteractiveState))
//! is already a pure function of state → ratatui `Buffer`. This harness wraps
//! that in the three layers iterating-a-TUI-blind needs:
//!
//! 1. **Scenario** — [`Screenplay::new`] builds an `InteractiveState` at a chosen
//!    width×height with no daemon/network behind it.
//! 2. **Drive** — [`Screenplay::type_str`] / [`Screenplay::key`] /
//!    [`Screenplay::event`] push through the *real* input + event handlers, so a
//!    snapshot reflects production code, not a synthetic shortcut.
//! 3. **Serialize** — [`Screenplay::render`] draws to a ratatui `TestBackend` and
//!    frames the buffer with a column ruler + row gutter + border, so wrap/layout
//!    bugs are *visible* in the diff (you can see exactly where column N falls).
//!    [`Screenplay::snapshot`] hands that to `insta` as a committed `.snap`.

use crossbeam_channel::{Sender, unbounded};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{Terminal, backend::TestBackend, buffer::Buffer};
use tokio::sync::mpsc::{UnboundedReceiver, unbounded_channel};
use unicode_width::UnicodeWidthStr;

use super::TuiEvent;
use super::render_context::{OutputMode, RenderContext};
use super::state::InteractiveState;
use super::theme::InteractiveTheme;
use crate::frontend::Approval;

/// A scripted TUI session: build state, drive input/events, snapshot the screen.
///
/// The lifetime is the projected document's; the no-arg constructor starts with
/// an empty document (`'static`), which is all the composer scenarios need. The
/// channel far-ends are held so sends from inside the state never error.
pub(crate) struct Screenplay<'a> {
    state: InteractiveState<'a>,
    width: u16,
    height: u16,
    _event_tx: Sender<TuiEvent>,
    _submit_rx: UnboundedReceiver<String>,
    _approve_rx: UnboundedReceiver<(String, Approval)>,
}

impl Screenplay<'static> {
    /// A fresh session at the given terminal size, empty transcript, default theme.
    pub(crate) fn new(width: u16, height: u16) -> Self {
        let rc = RenderContext::new()
            .with_interactive(true)
            .with_output_mode(OutputMode::Tty)
            .with_terminal_width(width as usize);
        let theme = InteractiveTheme::from_render_context(&rc);
        let (event_tx, event_rx) = unbounded();
        let (submit_tx, submit_rx) = unbounded_channel();
        let (approve_tx, approve_rx) = unbounded_channel();
        let state = InteractiveState::new(event_rx, submit_tx, approve_tx, rc, theme);
        Self {
            state,
            width,
            height,
            _event_tx: event_tx,
            _submit_rx: submit_rx,
            _approve_rx: approve_rx,
        }
    }
}

impl Screenplay<'_> {
    /// Type literal text into the composer, one key at a time, through the real
    /// `Composer::handle_key`. `\n` inserts a newline (Alt-Enter), matching the
    /// composer's multiline key rather than submitting.
    pub(crate) fn type_str(&mut self, text: &str) -> &mut Self {
        for ch in text.chars() {
            let key = if ch == '\n' {
                KeyEvent::new(KeyCode::Enter, KeyModifiers::ALT)
            } else {
                KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE)
            };
            self.state.composer.handle_key(key);
        }
        self
    }

    /// Send one key to the composer (e.g. `KeyCode::Backspace`, `Ctrl-A`).
    pub(crate) fn key(&mut self, code: KeyCode, mods: KeyModifiers) -> &mut Self {
        self.state.composer.handle_key(KeyEvent::new(code, mods));
        self
    }

    /// Feed a [`TuiEvent`] through the real handler — committed entries, streaming
    /// deltas, gate changes — exactly as the daemon bridge would.
    pub(crate) fn event(&mut self, event: TuiEvent) -> &mut Self {
        self.state.handle_event(event);
        self
    }

    /// Render the current state to a framed character grid. Idempotent — call it
    /// as many times as you like; it never mutates observable state.
    pub(crate) fn render(&mut self) -> String {
        let mut terminal = Terminal::new(TestBackend::new(self.width, self.height)).unwrap();
        let state = &mut self.state;
        terminal.draw(|frame| state.render_frame(frame)).unwrap();
        // The terminal cursor is backend state, not a buffer cell — capture it so
        // the grid can mark where the caret actually is (the composer sets it via
        // `Frame::set_cursor_position` while composing).
        let cursor = terminal.get_cursor_position().ok().map(|p| (p.x, p.y));
        frame_grid(terminal.backend().buffer(), self.width, cursor)
    }

    /// Render and assert against a committed `insta` file snapshot.
    pub(crate) fn snapshot(&mut self, name: &str) {
        insta::assert_snapshot!(name, self.render());
    }
}

/// Format a ratatui [`Buffer`] as a bordered, ruled character grid:
///
/// ```text
///      0         1
///      0123456789012345     <- column ruler (tens row shown when width > 9)
///    ┌────────────────┐
///  0 │› hello world   │     <- row gutter | content padded to width | border
///  1 │                │
///    └────────────────┘
/// ```
///
/// The ruler + border make wrap/overflow bugs legible: you can see precisely
/// which column a break lands on. Colors are intentionally dropped — only the
/// cell symbols are captured. Trailing all-blank rows are trimmed for focus.
///
/// When `cursor` is given, a caret line is drawn directly beneath its row with a
/// `^` under the exact column — non-destructive (never hides the cell under the
/// cursor) and unambiguous about where the insertion point sits.
fn frame_grid(buf: &Buffer, width: u16, cursor: Option<(u16, u16)>) -> String {
    let w = width as usize;

    let mut rows: Vec<String> = Vec::with_capacity(buf.area.height as usize);
    for y in 0..buf.area.height {
        let mut s = String::new();
        for x in 0..buf.area.width {
            s.push_str(buf[(x, y)].symbol());
        }
        rows.push(s.trim_end().to_string());
    }
    while rows.last().is_some_and(|r| r.is_empty()) {
        rows.pop();
    }

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

        if let Some((cx, cy)) = cursor {
            if cy as usize == y {
                let col = (cx as usize).min(w.saturating_sub(1));
                out.push_str("   │");
                out.push_str(&" ".repeat(col));
                out.push('^');
                out.push_str(&" ".repeat(w - col - 1));
                out.push_str("│\n");
            }
        }
    }

    // Bottom border.
    out.push_str(gutter);
    out.push('└');
    out.push_str(&"─".repeat(w));
    out.push_str("┘\n");
    out
}
