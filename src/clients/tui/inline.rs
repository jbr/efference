//! The inline viewport the pre-TUI prompts draw in (docs/tui.md).
//!
//! Below the shell prompt rather than in the alternate screen, because the
//! prompts run *before* a session exists: there is no transcript for a modal to
//! sit over, and an inline viewport leaves the shell as it found it. The
//! viewport's height is fixed at creation (ratatui reads a resize as the
//! *terminal's* size for `Viewport::Inline`), so a prompt sizes itself to its
//! tallest page before opening. Two prompts in a row each open and close their
//! own; closing erases from the viewport's origin down, so the next one starts
//! where the last one began.
//!
//! [`Inline`] holds raw mode, bracketed paste, the terminal, and the cursor
//! restoration that has to happen however the prompt ends. [`styles`] is the
//! TUI theme's foregrounds, the one palette every prompt draws with.

pub use crate::clients::tui::session_form::render::Styles;
use crate::clients::tui::{render_context::RenderContext, theme::InteractiveTheme};
use crossterm::{
    cursor::{MoveToColumn, Show},
    event::{DisableBracketedPaste, EnableBracketedPaste},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode},
};
use ratatui::{Terminal, TerminalOptions, backend::CrosstermBackend, buffer::Buffer, layout::Rect};
use std::io::{self, IsTerminal, stdout};

/// Whether stdin and stdout are a terminal an inline prompt can draw in.
pub fn interactive() -> bool {
    io::stdin().is_terminal() && stdout().is_terminal()
}

/// [`interactive`] as an error, for prompts with no non-interactive fallback.
pub fn require_terminal() -> io::Result<()> {
    match interactive() {
        true => Ok(()),
        false => Err(io::Error::other("the prompt needs an interactive terminal")),
    }
}

/// How many lines the terminal has, for bounding a viewport's height.
pub fn terminal_rows() -> u16 {
    crossterm::terminal::size().map_or(24, |(_, rows)| rows)
}

/// The TUI theme `theme` names (or the default), as the prompts' styles.
pub fn styles(theme: Option<String>) -> anyhow::Result<Styles> {
    let context =
        RenderContext::new().with_theme_name(&crate::clients::tui::run::resolve_theme(theme)?)?;
    Ok(Styles::from(&InteractiveTheme::from_render_context(
        &context,
    )))
}

/// Raw mode, the inline terminal, and the cursor restoration.
pub struct Inline {
    terminal: Terminal<CrosstermBackend<io::Stdout>>,
    origin: ratatui::layout::Position,
    _raw: Raw,
}

impl Inline {
    /// Take over the terminal with a viewport `height` lines tall.
    pub fn open(height: u16) -> io::Result<Self> {
        let _raw = Raw::enter()?;
        let terminal = Terminal::with_options(
            CrosstermBackend::new(stdout()),
            TerminalOptions {
                viewport: ratatui::Viewport::Inline(height),
            },
        )?;
        Ok(Self {
            terminal,
            origin: ratatui::layout::Position::ORIGIN,
            _raw,
        })
    }

    pub fn draw(&mut self, render: impl FnOnce(&mut Buffer, Rect)) -> io::Result<()> {
        self.terminal.draw(|frame| {
            let area = frame.area();
            self.origin = area.as_position();
            render(frame.buffer_mut(), area);
        })?;
        Ok(())
    }

    /// Inline clear erases only from the viewport's origin down, not scrollback,
    /// so the prompt leaves the shell exactly as it found it.
    pub fn close(mut self) -> io::Result<()> {
        self.terminal.clear()?;
        self.terminal.set_cursor_position(self.origin)
    }
}

impl From<&InteractiveTheme> for Styles {
    fn from(theme: &InteractiveTheme) -> Self {
        // Foregrounds and modifiers only. The theme's styles are built for the
        // full-screen TUI, where each carries its surface's background (the
        // status bar, the breadcrumb band); lent to a prompt they painted a band
        // under every gutter, a darker box under each source label, and a filled
        // blank status row. A prompt's surface is whatever it is drawn on — the
        // terminal's own background inline, the modal's block embedded.
        let fg_only = |style: ratatui::style::Style| ratatui::style::Style { bg: None, ..style };
        Self {
            text: ratatui::style::Style::default(),
            title: fg_only(theme.help_title_style),
            hint: fg_only(theme.status_hint_style),
            // The accent, as the selected row: the breadcrumb's own foreground
            // was chosen against the breadcrumb band, and without that band it
            // need not contrast with the terminal's background.
            selected: ratatui::style::Style {
                fg: theme.help_title_style.fg,
                ..Default::default()
            },
        }
    }
}

struct Raw;
impl Raw {
    fn enter() -> io::Result<Self> {
        enable_raw_mode()?;
        let guard = Self;
        execute!(stdout(), EnableBracketedPaste)?;
        Ok(guard)
    }
}
impl Drop for Raw {
    fn drop(&mut self) {
        let _ = execute!(stdout(), DisableBracketedPaste, Show, MoveToColumn(0));
        let _ = disable_raw_mode();
    }
}
