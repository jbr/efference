//! Inline viewport lifecycle for the fixed-data experiment (docs/tui.md).

use super::{Action, Form, HEIGHT, render::Styles};
use crate::tui::{DEFAULT_THEME, render_context::RenderContext, theme::InteractiveTheme};
use crossterm::{
    cursor::{MoveToColumn, Show},
    event::{self, DisableBracketedPaste, EnableBracketedPaste, Event},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode},
};
use ratatui::{Terminal, TerminalOptions, Viewport, backend::CrosstermBackend};
use std::io::{self, IsTerminal, stdout};

/// Draw below the shell prompt without entering the alternate screen. The
/// fixture never contacts a host; accepting prints the draft for inspection.
pub fn run() -> io::Result<()> {
    if !io::stdin().is_terminal() || !stdout().is_terminal() {
        return Err(io::Error::other(
            "the form prototype needs an interactive terminal",
        ));
    }
    let context = RenderContext::new()
        .with_theme_name(DEFAULT_THEME)
        .map_err(io::Error::other)?;
    let theme = InteractiveTheme::from_render_context(&context);
    let styles = Styles::from(&theme);
    let _raw = Raw::enter()?;
    let mut terminal = Terminal::with_options(
        CrosstermBackend::new(stdout()),
        TerminalOptions {
            viewport: Viewport::Inline(HEIGHT),
        },
    )?;
    let mut form = Form::default();
    let mut origin = ratatui::layout::Position::ORIGIN;
    let result = (|| {
        loop {
            terminal.draw(|frame| {
                let area = frame.area();
                origin = area.as_position();
                form.render(frame.buffer_mut(), area, styles);
            })?;
            let action = match event::read()? {
                Event::Key(key) => form.handle(key),
                Event::Paste(text) => {
                    form.paste(&text);
                    Action::Continue
                }
                _ => Action::Continue,
            };
            if action != Action::Continue {
                break Ok::<_, io::Error>(action);
            }
        }
    })();
    // Inline clear erases only from the viewport's origin down, not scrollback.
    let cleared = terminal
        .clear()
        .and_then(|()| terminal.set_cursor_position(origin));
    drop(terminal);
    drop(_raw);
    let action = result?;
    cleared?;
    match action {
        Action::Start(draft) => println!("Prototype accepted (no session created): {draft:?}"),
        _ => println!("Prototype cancelled."),
    }
    Ok(())
}

impl From<&InteractiveTheme> for Styles {
    fn from(theme: &InteractiveTheme) -> Self {
        Self {
            text: theme.help_bg_style,
            title: theme.help_title_style,
            hint: theme.status_hint_style,
            selected: theme.breadcrumb_style,
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
