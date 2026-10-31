//! The form's inline entry points (docs/tui.md).
//!
//! The viewport lifecycle is `tui/inline.rs`; it is sized here to the tallest
//! page the form can reach ([`Form::viewport_height`]).
//!
//! Two entry points share that lifecycle. [`run_hosted`] is the real one: the
//! picker's "create a new session" (`bin/efference/picker.rs`), wired to a host
//! through the same dispatcher the full-screen client uses. [`run`] is
//! `efference dev session-form`, which draws the same [`Form`] with no socket
//! behind it so the layout and key transitions can be walked offline.

use super::{Action, Form, NewSessionContext, render::Styles};
use crate::client::Client;
use crate::tui::inline::{self, Inline};
use crate::tui::{HostCmd, TuiEvent};
use crossterm::event::{self, Event};
use std::io;
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
    inline::require_terminal()?;
    let styles = inline::styles(theme)?;
    let cwd = std::env::current_dir()?;
    let context = crate::tui::host_dispatch::new_session_context(&client, &cwd)
        .await
        .map_err(anyhow::Error::msg)?;
    let (events, replies) = crossbeam_channel::unbounded();
    let (commands, command_rx) = tokio::sync::mpsc::unbounded_channel();
    // The stored copies that are not fresh are read as the form opens, the same
    // reads the embedded container's dispatcher starts after `NewSession`.
    crate::tui::host_dispatch::spawn_catalog_reads(
        &client,
        &cwd,
        crate::tui::host_dispatch::unfresh_connections(&context.catalog),
        None,
        &events,
    );
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
    let mut viewport = Inline::open(form.viewport_height())?;
    // Resolve the untouched draft, so the form opens showing the defaults a
    // session would actually get rather than blanks.
    dispatch(&commands, form.initial_preview());
    let outcome = (|| {
        loop {
            viewport.draw(|buf, area| form.render(buf, area, styles))?;
            // A reply that lands between keystrokes still has to repaint, so the
            // key read is a poll rather than a block.
            while let Ok(reply) = replies.try_recv() {
                match reply {
                    TuiEvent::Preview(resolved) => {
                        dispatch(&commands, form.preview_returned(resolved));
                    }
                    TuiEvent::Catalog { connection, result } => {
                        form.catalog_returned(&connection, result);
                    }
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
                Event::Paste(text) => form.paste(&text),
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
        Action::Create { options, worktree } => {
            commands.send(HostCmd::Create { options, worktree })
        }
        Action::RefreshCatalog {
            connection,
            max_age,
        } => commands.send(HostCmd::Catalog {
            connection,
            max_age,
        }),
        Action::Continue | Action::Cancel => Ok(()),
    };
}

/// Draw below the shell prompt with no host behind it: the layout and the key
/// transitions, without a socket. Accepting prints the draft for inspection.
pub fn run() -> io::Result<()> {
    inline::require_terminal()?;
    let styles = inline::styles(None).map_err(io::Error::other)?;
    // No host to read a context from, so the root is this process's — the same
    // one a wired container would send — and the connection list is empty.
    let mut form = Form::new(NewSessionContext {
        cwd: std::env::current_dir().unwrap_or_default(),
        connections: Vec::new(),
        catalog: Vec::new(),
    });
    let mut viewport = Inline::open(form.viewport_height())?;
    let outcome = (|| {
        loop {
            viewport.draw(|buf, area| form.render(buf, area, styles))?;
            let action = match event::read()? {
                Event::Key(key) => form.handle(key),
                Event::Paste(text) => form.paste(&text),
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
        Action::Create { options, worktree } => {
            println!(
                "Accepted (no host attached, so nothing was created): {options:?} · worktree: {worktree}"
            );
        }
        _ => println!("Cancelled."),
    }
    Ok(())
}
