//! Inline viewport lifecycle for the form (docs/tui.md).
//!
//! Below the shell prompt rather than in the alternate screen, because this runs
//! *before* a session exists: there is no transcript for a modal to sit over,
//! and leaving the form's rows in scrollback is how `efference setup`'s prompts
//! behave. The viewport is sized to the tallest page the form can reach
//! ([`Form::viewport_height`]) — ratatui fixes an inline viewport's height at
//! creation.
//!
//! Two entry points share that lifecycle. [`run_hosted`] is the real one: the
//! picker's "create a new session" (`bin/efference/picker.rs`), wired to a host
//! through the same dispatcher the full-screen client uses. [`run`] is
//! `efference dev session-form`, which draws the same [`Form`] with no socket
//! behind it so the layout and key transitions can be walked offline.

use super::{Action, Form, NewSessionContext, render::Styles};
use crate::client::Client;
use crate::tui::{HostCmd, TuiEvent, render_context::RenderContext, theme::InteractiveTheme};
use crossterm::{
    cursor::{MoveToColumn, Show},
    event::{self, DisableBracketedPaste, EnableBracketedPaste, Event},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode},
};
use ratatui::{Terminal, TerminalOptions, backend::CrosstermBackend};
use std::io::{self, IsTerminal, stdout};
use std::time::Duration;

/// How long the loop waits for a keypress before looking at the host's replies.
/// Short enough that a landed preview repaints without a keystroke to prompt it.
const TICK: Duration = Duration::from_millis(80);

/// Create a session through the form, returning the new session's id — or
/// `None` if the operator cancelled without creating one.
///
/// The connection inventory and the working root are read *before* the terminal
/// is taken over, because the viewport's height depends on how many connections
/// there are and an inline viewport cannot be resized afterwards. Everything
/// after that goes through the same [`host_dispatch`](crate::tui::host_dispatch)
/// task the full-screen client uses, so preview and creation cannot drift
/// between the two surfaces.
pub async fn run_hosted(client: Client, theme: Option<String>) -> anyhow::Result<Option<String>> {
    require_terminal()?;
    let styles = styles(theme)?;
    let cwd = std::env::current_dir()?;
    let context = crate::tui::host_dispatch::new_session_context(&client, &cwd)
        .await
        .map_err(anyhow::Error::msg)?;
    let (events, replies) = crossbeam_channel::unbounded();
    let (commands, command_rx) = tokio::sync::mpsc::unbounded_channel();
    let dispatcher = tokio::spawn(crate::tui::host_dispatch::serve(client, command_rx, events));
    let created =
        tokio::task::spawn_blocking(move || drive(Form::new(context), styles, replies, commands))
            .await??;
    dispatcher.abort();
    Ok(created)
}

/// The blocking half: draw, read a key, and fold in whatever the host answered.
/// Returns the created session's id, or `None` for a cancel.
fn drive(
    mut form: Form,
    styles: Styles,
    replies: crossbeam_channel::Receiver<TuiEvent>,
    commands: tokio::sync::mpsc::UnboundedSender<HostCmd>,
) -> io::Result<Option<String>> {
    let mut viewport = Inline::open(&form)?;
    // Resolve the untouched draft, so the form opens showing the defaults a
    // session would actually get rather than blanks.
    dispatch(&commands, form.initial_preview());
    let outcome = (|| {
        loop {
            viewport.draw(&form, styles)?;
            // A reply that lands between keystrokes still has to repaint, so the
            // key read is a poll rather than a block.
            while let Ok(reply) = replies.try_recv() {
                match reply {
                    TuiEvent::Preview(resolved) => {
                        dispatch(&commands, form.preview_returned(resolved));
                    }
                    TuiEvent::Recents(Ok((role, connection, models))) => {
                        dispatch(&commands, form.recents_returned(role, connection, models));
                    }
                    TuiEvent::Recents(Err(error)) => form.note_recents_failed(error),
                    TuiEvent::Created(Ok(id)) => return Ok(Some(id)),
                    TuiEvent::Created(Err(error)) => form.creation_failed(error),
                    // The context was read before the loop started, and nothing
                    // here subscribes to a session's stream.
                    _ => {}
                }
            }
            if !event::poll(TICK)? {
                continue;
            }
            let action = match event::read()? {
                Event::Key(key) => form.handle(key),
                Event::Paste(text) => {
                    form.paste(&text);
                    Action::Continue
                }
                _ => Action::Continue,
            };
            if matches!(action, Action::Cancel) {
                return Ok(None);
            }
            dispatch(&commands, action);
        }
    })();
    viewport.close()?;
    outcome
}

/// Turn what a keypress asked for into a host command. The form issues no
/// requests itself (`session_form.rs`).
pub(super) fn dispatch(commands: &tokio::sync::mpsc::UnboundedSender<HostCmd>, action: Action) {
    let _ = match action {
        Action::Preview(options) => commands.send(HostCmd::Preview(options)),
        Action::Create(options) => commands.send(HostCmd::Create(options)),
        Action::Recents(request) => commands.send(HostCmd::Recents(request)),
        Action::Continue | Action::Cancel => Ok(()),
    };
}

/// Draw below the shell prompt with no host behind it: the layout and the key
/// transitions, without a socket. Accepting prints the draft for inspection.
pub fn run() -> io::Result<()> {
    require_terminal()?;
    let styles = styles(None).map_err(io::Error::other)?;
    // No host to read a context from, so the root is this process's — the same
    // one a wired container would send — and the connection list is empty.
    let mut form = Form::new(NewSessionContext {
        cwd: std::env::current_dir().unwrap_or_default(),
        connections: Vec::new(),
        recent_picks: Vec::new(),
    });
    let mut viewport = Inline::open(&form)?;
    let outcome = (|| {
        loop {
            viewport.draw(&form, styles)?;
            let action = match event::read()? {
                Event::Key(key) => form.handle(key),
                Event::Paste(text) => {
                    form.paste(&text);
                    Action::Continue
                }
                _ => Action::Continue,
            };
            match action {
                Action::Continue => {}
                // Nothing will answer a preview here, which the form states
                // rather than leaving the fields looking merely unset.
                Action::Preview(_) => {}
                other => break Ok::<_, io::Error>(other),
            }
        }
    })();
    viewport.close()?;
    match outcome? {
        Action::Create(options) => {
            println!("Accepted (no host attached, so nothing was created): {options:?}")
        }
        _ => println!("Cancelled."),
    }
    Ok(())
}

fn require_terminal() -> io::Result<()> {
    match io::stdin().is_terminal() && stdout().is_terminal() {
        true => Ok(()),
        false => Err(io::Error::other("the form needs an interactive terminal")),
    }
}

fn styles(theme: Option<String>) -> anyhow::Result<Styles> {
    let context = RenderContext::new().with_theme_name(&crate::tui::run::resolve_theme(theme)?)?;
    Ok(Styles::from(&InteractiveTheme::from_render_context(
        &context,
    )))
}

/// Raw mode, the inline terminal, and the cursor restoration that has to happen
/// however the loop ends. Held by both entry points so neither can forget half
/// of it.
struct Inline {
    terminal: Terminal<CrosstermBackend<io::Stdout>>,
    origin: ratatui::layout::Position,
    _raw: Raw,
}

impl Inline {
    fn open(form: &Form) -> io::Result<Self> {
        let _raw = Raw::enter()?;
        let terminal = Terminal::with_options(
            CrosstermBackend::new(stdout()),
            TerminalOptions {
                viewport: ratatui::Viewport::Inline(form.viewport_height()),
            },
        )?;
        Ok(Self {
            terminal,
            origin: ratatui::layout::Position::ORIGIN,
            _raw,
        })
    }

    fn draw(&mut self, form: &Form, styles: Styles) -> io::Result<()> {
        self.terminal.draw(|frame| {
            let area = frame.area();
            self.origin = area.as_position();
            form.render(frame.buffer_mut(), area, styles);
        })?;
        Ok(())
    }

    /// Inline clear erases only from the viewport's origin down, not scrollback,
    /// so the form leaves the shell exactly as it found it.
    fn close(mut self) -> io::Result<()> {
        self.terminal.clear()?;
        self.terminal.set_cursor_position(self.origin)
    }
}

impl From<&InteractiveTheme> for Styles {
    fn from(theme: &InteractiveTheme) -> Self {
        // Foregrounds and modifiers only. The theme's styles are built for the
        // full-screen TUI, where each carries its surface's background (the
        // status bar, the breadcrumb band); lent to the form they painted a band
        // under every gutter, a darker box under each source label, and a filled
        // blank status row. The form's surface is whatever it is drawn on — the
        // terminal's own background inline, the modal's block embedded.
        let fg_only = |style: ratatui::style::Style| ratatui::style::Style { bg: None, ..style };
        Self {
            text: ratatui::style::Style::default(),
            title: fg_only(theme.help_title_style),
            hint: fg_only(theme.status_hint_style),
            // The accent, as the picker's selected row: the breadcrumb's own
            // foreground was chosen against the breadcrumb band, and without that
            // band it need not contrast with the terminal's background.
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
