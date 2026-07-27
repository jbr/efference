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
use ratatui::{
    Terminal,
    backend::TestBackend,
    buffer::Buffer,
    style::{Color, Modifier},
};
use tokio::sync::mpsc::{UnboundedReceiver, unbounded_channel};

use super::TuiEvent;
use super::grid::frame_grid;
use super::render_context::{OutputMode, RenderContext};
use super::state::InteractiveState;
use super::theme::InteractiveTheme;
use crate::frontend::Approval;
use crate::log::{
    CallId, ContentBlock, LogEntry, Payload, RawResult, Stance, Timestamp, ToolCall, WireState,
};
use serde_json::{Value, json};

/// A scripted TUI session: build state, drive input/events, snapshot the screen.
///
/// The lifetime is the projected document's; the no-arg constructor starts with
/// an empty document (`'static`), which is all the composer scenarios need. The
/// channel far-ends are held so sends from inside the state never error.
pub(crate) struct Screenplay<'a> {
    /// `pub(super)`, not `pub(crate)`: it exposes `InteractiveState`, which is
    /// itself `pub(in crate::tui)` — and every scenario lives inside `tui`.
    pub(super) state: InteractiveState<'a>,
    width: u16,
    height: u16,
    next_seq: u64,
    _event_tx: Sender<TuiEvent>,
    _submit_rx: UnboundedReceiver<String>,
    /// Held un-underscored so a scenario can assert what decision a gate action
    /// sent ([`took_approval`](Screenplay::took_approval)).
    approve_rx: UnboundedReceiver<(String, Approval)>,
    /// Held un-underscored so a scenario can assert the pace command a widget
    /// commit pushed ([`took_pace`](Screenplay::took_pace)).
    pace_rx: UnboundedReceiver<super::PaceCmd>,
    /// Held un-underscored so a scenario can assert an interrupt intent fired —
    /// the hard rung of graduated Esc ([`took_interrupt`](Screenplay::took_interrupt))
    /// or the background handoff ([`took_background`](Screenplay::took_background)).
    interrupt_rx: UnboundedReceiver<crate::frontend::Interrupt>,
}

impl Screenplay<'static> {
    /// A fresh session at the given terminal size, empty transcript, default theme.
    pub(crate) fn new(width: u16, height: u16) -> Self {
        // Load the same theme the real app defaults to, so the harness sees the
        // production color scheme (`RenderContext::new` alone leaves an empty
        // `ColorScheme` where every semantic style falls back to one gray). Snapshots
        // drop color, so this only matters to `cell`-based style assertions — but it
        // makes those faithful to what the user actually sees.
        let rc = RenderContext::new()
            .with_interactive(true)
            .with_output_mode(OutputMode::Tty)
            .with_terminal_width(width as usize)
            .with_theme_name(super::DEFAULT_THEME)
            .expect("the default theme loads");
        let theme = InteractiveTheme::from_render_context(&rc);
        let (event_tx, event_rx) = unbounded();
        let (submit_tx, submit_rx) = unbounded_channel();
        let (approve_tx, approve_rx) = unbounded_channel();
        let (pace_tx, pace_rx) = unbounded_channel();
        let (interrupt_tx, interrupt_rx) = unbounded_channel();
        let state = InteractiveState::new(
            event_rx,
            submit_tx,
            approve_tx,
            pace_tx,
            interrupt_tx,
            rc,
            theme,
        );
        Self {
            state,
            width,
            height,
            next_seq: 1,
            _event_tx: event_tx,
            _submit_rx: submit_rx,
            approve_rx,
            pace_rx,
            interrupt_rx,
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

    /// Route a key through the *full* [`InteractiveState::handle_key_event`] — mode
    /// routing, the composer `↑`-spill, Esc-home, unit navigation — exactly as the
    /// `ui_loop` drives it, against an off-screen `TestBackend` terminal. Use this
    /// (not [`key`](Self::key), which talks straight to the composer) to exercise
    /// focus transitions.
    pub(crate) fn route_key(&mut self, code: KeyCode, mods: KeyModifiers) -> &mut Self {
        let _ = self.route_key_inner(code, mods);
        self
    }

    /// Like [`route_key`](Self::route_key) but returns `handle_key_event`'s quit
    /// signal — for asserting a key exits (e.g. C-d on an empty composer).
    pub(crate) fn key_quits(&mut self, code: KeyCode, mods: KeyModifiers) -> bool {
        self.route_key_inner(code, mods)
    }

    fn route_key_inner(&mut self, code: KeyCode, mods: KeyModifiers) -> bool {
        // `handle_key_event` needs `Backend + Write` (the mouse/theme `execute!`s);
        // `TestBackend` isn't `Write`, so back it with a `CrosstermBackend` over a
        // `Vec<u8>` sink. A *fixed* viewport avoids querying a real tty at
        // construction (there is none under `cargo test`).
        use ratatui::{TerminalOptions, Viewport, backend::CrosstermBackend, layout::Rect};
        // The `ui_loop` always paints a fresh frame before handling a key, which is
        // what populates `render_cache.actions` + the layout cache the navigation
        // logic reads. Mirror that here, or focus has nothing to land on.
        let _ = self.draw();
        let backend = CrosstermBackend::new(Vec::<u8>::new());
        let mut term = Terminal::with_options(
            backend,
            TerminalOptions {
                viewport: Viewport::Fixed(Rect::new(0, 0, self.width, self.height)),
            },
        )
        .unwrap();
        self.state
            .handle_key_event(KeyEvent::new(code, mods), &mut term)
    }

    /// Feed a [`TuiEvent`] through the real handler — committed entries, streaming
    /// deltas, gate changes — exactly as the daemon bridge would.
    pub(crate) fn event(&mut self, event: TuiEvent) -> &mut Self {
        self.state.handle_event(event);
        self
    }

    /// Whether the hard rung of graduated Esc fired a `POST /interrupt` since the last check
    /// (docs/tui.md) — drains the interrupt channel the UI would have
    /// sent on.
    pub(crate) fn took_interrupt(&mut self) -> bool {
        matches!(
            self.interrupt_rx.try_recv(),
            Ok(crate::frontend::Interrupt::Kill)
        )
    }

    /// Whether the background-handoff key fired a `POST /background` since the last
    /// check (docs/supervisor.md) — the detach intent on the same channel.
    pub(crate) fn took_background(&mut self) -> bool {
        matches!(
            self.interrupt_rx.try_recv(),
            Ok(crate::frontend::Interrupt::Detach)
        )
    }

    /// The approval decision a gate action sent since the last check (`call_id`,
    /// decision), if any — drains the channel the UI would have resolved
    /// `POST /approvals/:call_id` over.
    pub(crate) fn took_approval(&mut self) -> Option<(String, Approval)> {
        self.approve_rx.try_recv().ok()
    }

    /// The pace command (`speed_ms`, `held`) pushed since the last check, if any —
    /// drains the channel the UI would `POST /pace` over. `None` means nothing was
    /// sent (e.g. a widget preview or revert, which never touch the daemon).
    pub(crate) fn took_pace(&mut self) -> Option<super::PaceCmd> {
        self.pace_rx.try_recv().ok()
    }

    /// Click screen cell `(x, y)` through the real mouse path — the press that
    /// records a target, then the deferred `handle_click` the UI loop runs.
    pub(crate) fn click(&mut self, x: u16, y: u16) -> &mut Self {
        use crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
        // As with keys: paint first, so the frame's hit maps exist to resolve against.
        let _ = self.draw();
        let terminal = Terminal::new(TestBackend::new(self.width, self.height)).unwrap();
        self.state.handle_mouse_event(
            MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: x,
                row: y,
                modifiers: KeyModifiers::NONE,
            },
            &terminal,
        );
        self.state.handle_click();
        self
    }

    /// Draw the current state to a fresh `TestBackend` and return the resulting
    /// buffer plus the terminal cursor position. Idempotent — never mutates
    /// observable state. The shared primitive behind [`render`](Self::render) /
    /// [`cell`](Self::cell) / [`find`](Self::find).
    fn draw(&mut self) -> (Buffer, Option<(u16, u16)>) {
        let mut terminal = Terminal::new(TestBackend::new(self.width, self.height)).unwrap();
        // Mirror the UI loop: events only flag a pending reproject, so realise it
        // before painting (the loop calls `flush_reproject` before every draw).
        self.state.flush_reproject();
        let state = &mut self.state;
        terminal.draw(|frame| state.render_frame(frame)).unwrap();
        // The terminal cursor is backend state, not a buffer cell — capture it so
        // the grid can mark where the caret actually is (the composer sets it via
        // `Frame::set_cursor_position` while composing).
        let cursor = terminal.get_cursor_position().ok().map(|p| (p.x, p.y));
        (terminal.backend().buffer().clone(), cursor)
    }

    /// Render the current state to a framed character grid.
    pub(crate) fn render(&mut self) -> String {
        let (buf, cursor) = self.draw();
        frame_grid(&buf, self.width, cursor)
    }

    /// The live "dev screenshot" capture path (`Ctrl+S`), exercised headlessly: it
    /// renders through [`InteractiveState::capture_screenshot_grid`] exactly as the
    /// keybind does. Must equal [`render`](Self::render) byte-for-byte — that
    /// equality is what makes a live screenshot diffable against a committed `.snap`.
    pub(crate) fn screenshot_grid(&mut self) -> String {
        self.state.capture_screenshot_grid(self.width, self.height)
    }

    /// The `Alt-S` sidebar-only capture path, headless — frames just the sidebar
    /// columns (docs/tui.md).
    pub(crate) fn sidebar_grid(&mut self) -> String {
        self.state.capture_sidebar_grid(self.width, self.height)
    }

    /// Pin the viewport to the bottom — the way a stream-following TUI sits while a
    /// session is live (`stuck_to_bottom`). Bottom-pinning normally happens in the
    /// `ui_loop` after a reproject; the harness has no loop, so this draws once to
    /// populate the cached layout, then scrolls to the computed max.
    pub(crate) fn pin_to_bottom(&mut self) -> &mut Self {
        let _ = self.draw();
        self.state.scroll_to_bottom();
        self
    }

    /// The symbol **and style** of the cell at buffer coords `(x, y)` — the same
    /// column/row the snapshot's ruler and gutter label. The character grid drops
    /// color by design (see [`frame_grid`]); this is the escape hatch to *view or
    /// assert* the foreground / background / modifiers (bold, italic, crossed-out)
    /// that a `.snap` can't show — e.g. confirming a diff's `+`/`-` lines are green
    /// and red. Pair with [`find`](Self::find) to locate a position by content.
    pub(crate) fn cell(&mut self, x: u16, y: u16) -> CellStyle {
        let (buf, _) = self.draw();
        let cell = &buf[(x, y)];
        CellStyle {
            symbol: cell.symbol().to_string(),
            fg: cell.fg,
            bg: cell.bg,
            modifier: cell.modifier,
        }
    }

    /// The `(x, y)` buffer position where `needle` first appears, scanning rows
    /// top-to-bottom. Returns the column of the match's first cell, so a style
    /// assertion can target text by content instead of a brittle hardcoded column.
    pub(crate) fn find(&mut self, needle: &str) -> Option<(u16, u16)> {
        let (buf, _) = self.draw();
        for y in 0..buf.area.height {
            let mut row = String::new();
            let mut starts: Vec<(usize, u16)> = Vec::new();
            for x in 0..buf.area.width {
                starts.push((row.len(), x));
                row.push_str(buf[(x, y)].symbol());
            }
            if let Some(byte) = row.find(needle) {
                let x = starts
                    .iter()
                    .rev()
                    .find(|(off, _)| *off <= byte)
                    .map(|(_, x)| *x)
                    .unwrap_or(0);
                return Some((x, y));
            }
        }
        None
    }

    /// Render and assert against a committed `insta` file snapshot.
    pub(crate) fn snapshot(&mut self, name: &str) {
        insta::assert_snapshot!(name, self.render());
    }
}

/// The symbol and visual style of one rendered cell — what a character-grid
/// snapshot can't capture. Returned by [`Screenplay::cell`]; `Debug`-print it to
/// *view* a cell, or read its fields to *assert* on color/modifiers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CellStyle {
    pub symbol: String,
    pub fg: Color,
    pub bg: Color,
    pub modifier: Modifier,
}

impl CellStyle {
    /// Whether this cell carries a given modifier (e.g. `Modifier::CROSSED_OUT`,
    /// `Modifier::BOLD`, `Modifier::ITALIC`).
    pub(crate) fn has(&self, modifier: Modifier) -> bool {
        self.modifier.contains(modifier)
    }
}

/// Build a [`ToolCall`] for a scenario: a tool name, the `toolu_…` id that its
/// result will reference, and a JSON input value (`serde_json::json!({…})`).
pub(crate) fn tool_call(name: &str, call_id: &str, input: serde_json::Value) -> ToolCall {
    ToolCall {
        id: CallId(call_id.into()),
        name: name.into(),
        input,
    }
}

/// Baseline transcript scenarios — realistic user / assistant / tool blocks
/// rendered through the full frame, so formatting/rendering changes have a
/// visible before/after. These are the "what the user sees" fixtures; the
/// composer-focused scenarios live in `composer.rs`.
#[cfg(test)]
mod test;

mod build;
