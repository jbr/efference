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

use super::SidebarCmd;
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
    /// Every intent channel's far end, held so a send from inside the state never
    /// errors — and read by the `took_*` accessors below, which is how a scenario
    /// asserts what a gesture actually dispatched.
    receivers: super::senders::Receivers,
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
        let (senders, receivers) = super::senders::Senders::channels();
        let state = InteractiveState::new(event_rx, senders, rc, theme);
        Self {
            state,
            width,
            height,
            next_seq: 1,
            _event_tx: event_tx,
            receivers,
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

    /// Replace the system clipboard with a test-only unavailable adapter, so
    /// routed copy/cut/paste scenarios cannot touch the host clipboard.
    pub(crate) fn disable_system_clipboard(&mut self) -> &mut Self {
        self.state.clipboard = super::clipboard::TextClipboard::unavailable();
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

    /// Route a fully specified crossterm key event. Test key construction differs
    /// from terminals that carry a `KeyEventKind`, so clipboard scenarios use this
    /// to exercise the same modifier path without host input.
    pub(crate) fn route_event(&mut self, event: KeyEvent) -> &mut Self {
        let _ = self.route_key_event_inner(event);
        self
    }

    /// Feed one full key event through the composer-focused dispatch without the
    /// global TUI bindings, for cases where terminal modifier encoding matters.
    pub(crate) fn composer_event(&mut self, event: KeyEvent) -> &mut Self {
        self.state.handle_composer_key(event);
        self
    }

    /// Like [`route_key`](Self::route_key) but returns `handle_key_event`'s quit
    /// signal — for asserting a key exits (e.g. C-d on an empty composer).
    pub(crate) fn key_quits(&mut self, code: KeyCode, mods: KeyModifiers) -> bool {
        self.route_key_inner(code, mods)
    }

    fn route_key_inner(&mut self, code: KeyCode, mods: KeyModifiers) -> bool {
        self.route_key_event_inner(KeyEvent::new(code, mods))
    }

    fn route_key_event_inner(&mut self, event: KeyEvent) -> bool {
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
        self.state.handle_key_event(event, &mut term)
    }

    /// Feed a [`TuiEvent`] through the real handler — committed entries, streaming
    /// deltas, gate changes — exactly as the daemon bridge would.
    pub(crate) fn event(&mut self, event: TuiEvent) -> &mut Self {
        self.state.handle_event(event);
        self
    }

    /// Whether folding this event in ended the UI loop — the sibling of
    /// [`key_quits`](Self::key_quits) for the events that can end it, as a
    /// creation does by switching to the session it just made.
    pub(crate) fn event_quits(&mut self, event: TuiEvent) -> bool {
        self.state.handle_event(event)
    }

    /// Whether the hard rung of graduated Esc fired a `POST /loop/interrupt` since the last check
    /// (docs/tui.md) — drains the interrupt channel the UI would have
    /// sent on.
    pub(crate) fn took_interrupt(&mut self) -> bool {
        matches!(
            self.receivers.interrupt.try_recv(),
            Ok(crate::frontend::Interrupt::Kill)
        )
    }

    /// Whether the background-handoff key fired a `PATCH /processes/:id {background: true}` since the last
    /// check (docs/supervisor.md) — the detach intent on the same channel.
    pub(crate) fn took_background(&mut self) -> bool {
        matches!(
            self.receivers.interrupt.try_recv(),
            Ok(crate::frontend::Interrupt::Detach)
        )
    }

    /// The sidebar verb dispatched since the last check, if any — drains the channel
    /// the UI would send a focused row's kill / commit / cancel over.
    pub(crate) fn took_sidebar_cmd(&mut self) -> Option<SidebarCmd> {
        self.receivers.sidebar.try_recv().ok()
    }

    /// The user turn a submit sent since the last check, if any — drains the channel
    /// the UI would `POST /turns` over. `None` means nothing was sent.
    pub(crate) fn took_turn(&mut self) -> Option<String> {
        self.receivers.submit.try_recv().ok().map(|turn| turn.text)
    }

    /// The attachment command a staging gesture dispatched since the last check, if
    /// any — drains the channel the UI would upload or remove over.
    pub(crate) fn took_attach(&mut self) -> Option<super::staged::AttachCmd> {
        self.receivers.attachments.try_recv().ok()
    }

    /// The whole turn a submit sent since the last check — text and attachments.
    pub(crate) fn took_submission(&mut self) -> Option<crate::frontend::SubmittedTurn> {
        self.receivers.submit.try_recv().ok()
    }

    /// The submission id a recall asked to withdraw since the last check, if any —
    /// drains the channel the UI would `DELETE /turns/:id` over.
    #[allow(dead_code)]
    pub(crate) fn took_recall(&mut self) -> Option<String> {
        self.receivers.recall.try_recv().ok()
    }

    /// Whether the empty-composer `⏎` fired the zero-token continue since the last
    /// check — drains the channel the UI would `POST /loop/continue` over.
    pub(crate) fn took_continue(&mut self) -> bool {
        self.receivers.continue_loop.try_recv().is_ok()
    }

    /// The approval decision a gate action sent since the last check (`call_id`,
    /// decision), if any — drains the channel the UI would have resolved
    /// `PUT /approvals/:call_id` over.
    pub(crate) fn took_approval(&mut self) -> Option<(String, Approval, bool)> {
        self.receivers.approve.try_recv().ok()
    }

    /// The pace command (`speed_ms`, `held`) pushed since the last check, if any —
    /// drains the channel the UI would `PATCH /loop {pace}` over. `None` means nothing was
    /// sent (e.g. a widget preview or revert, which never touch the daemon).
    /// The host verb a switcher gesture dispatched (`sessions.rs`), if any.
    pub(crate) fn took_host_cmd(&mut self) -> Option<super::HostCmd> {
        self.receivers.host.try_recv().ok()
    }

    pub(crate) fn took_pace(&mut self) -> Option<super::PaceCmd> {
        self.receivers.pace.try_recv().ok()
    }

    /// Move the pointer to screen cell `(x, y)` through the real mouse path — the
    /// `Moved` event a terminal sends, which is what sets hover state (the
    /// scrollbar's, and the control strip's engagement — docs/tui.md).
    pub(crate) fn hover(&mut self, x: u16, y: u16) -> &mut Self {
        use crossterm::event::{MouseEvent, MouseEventKind};
        let _ = self.draw();
        let terminal = Terminal::new(TestBackend::new(self.width, self.height)).unwrap();
        self.state.handle_mouse_event(
            MouseEvent {
                kind: MouseEventKind::Moved,
                column: x,
                row: y,
                modifiers: KeyModifiers::NONE,
            },
            &terminal,
        );
        self
    }

    /// The screen row the control strip occupies — where a hover or a click has to
    /// land to reach it. Derived the way the mouse handler derives it, so a
    /// scenario cannot aim at a row the production code would not accept.
    pub(crate) fn strip_row(&mut self) -> u16 {
        let _ = self.draw();
        self.height
            .saturating_sub(2 + self.state.viewport.input_height)
    }

    /// Click screen cell `(x, y)` through the real mouse path — the press that
    /// records a target, then the deferred `handle_click` the UI loop runs.
    pub(crate) fn click(&mut self, x: u16, y: u16) -> &mut Self {
        self.click_with_modifiers(x, y, KeyModifiers::NONE)
    }

    /// Click through the real mouse path with explicit modifiers.
    pub(crate) fn click_with_modifiers(
        &mut self,
        x: u16,
        y: u16,
        modifiers: KeyModifiers,
    ) -> &mut Self {
        use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
        // As with keys: paint first, so the frame's hit maps exist to resolve against.
        let _ = self.draw();
        let terminal = Terminal::new(TestBackend::new(self.width, self.height)).unwrap();
        self.state.handle_mouse_event(
            MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: x,
                row: y,
                modifiers,
            },
            &terminal,
        );
        self.state.handle_click();
        self
    }

    /// Drag from `(from_x, from_y)` to `(to_x, to_y)` through the real mouse path.
    /// The press, drag, and release share the same terminal geometry as live events.
    pub(crate) fn drag(&mut self, from_x: u16, from_y: u16, to_x: u16, to_y: u16) -> &mut Self {
        use crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
        let _ = self.draw();
        let terminal = Terminal::new(TestBackend::new(self.width, self.height)).unwrap();
        for (kind, column, row) in [
            (MouseEventKind::Down(MouseButton::Left), from_x, from_y),
            (MouseEventKind::Drag(MouseButton::Left), to_x, to_y),
            (MouseEventKind::Up(MouseButton::Left), to_x, to_y),
        ] {
            self.state.handle_mouse_event(
                MouseEvent {
                    kind,
                    column,
                    row,
                    modifiers: KeyModifiers::NONE,
                },
                &terminal,
            );
        }
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

    /// The `Alt-S` drawer-only capture path, headless — frames just the session
    /// drawer's columns (docs/tui.md), opened for the capture.
    pub(crate) fn sidebar_grid(&mut self) -> String {
        self.state.capture_sidebar_grid(self.width, self.height)
    }

    /// Open the session drawer without moving focus, so a snapshot shows it.
    pub(crate) fn open_drawer(&mut self) -> &mut Self {
        self.state.sidebar_open = true;
        self
    }

    /// Pin the viewport to the bottom — the way a stream-following TUI sits while a
    /// session is live (`stuck_to_bottom`, the default). `render_frame` re-pins on
    /// every paint, so this is only an explicit restatement of the default; keep it
    /// where a scenario means "following the stream" rather than "happens to be".
    pub(crate) fn pin_to_bottom(&mut self) -> &mut Self {
        self.state.viewport.stuck_to_bottom = true;
        let _ = self.draw();
        self.state.scroll_to_bottom();
        self
    }

    /// Detach from the stream and show the *head* of the transcript — what a human
    /// sees after scrolling up. Needed wherever a scenario snapshots content that a
    /// bottom-pinned viewport would have scrolled past.
    pub(crate) fn scroll_to_top(&mut self) -> &mut Self {
        self.state.viewport.stuck_to_bottom = false;
        self.state.set_scroll_offset(0);
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

/// Baseline transcript scenarios — realistic user / assistant / tool blocks
/// rendered through the full frame, so formatting/rendering changes have a
/// visible before/after. These are the "what the user sees" fixtures; the
/// composer-focused scenarios live in `composer.rs`.
#[cfg(test)]
mod test;

mod build;
