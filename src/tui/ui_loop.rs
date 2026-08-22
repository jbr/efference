//! The blocking crossterm UI loop: owns the terminal and renders each frame.

use super::*;

use crate::tui::frame_cost::FrameCost;
use ratatui::prelude::Backend;
use std::io::Write;

/// The five channels the UI sends the operator's intent over, carried together
/// because every entry point builds all five at once and hands them straight to the
/// state. The tokio side turns each into its control-channel call; an offline run
/// (the gallery, a read-only view) holds the far-ends so a send can't error.
pub(super) struct Senders {
    pub submit: tokio::sync::mpsc::UnboundedSender<String>,
    pub approve: tokio::sync::mpsc::UnboundedSender<(String, crate::frontend::Approval)>,
    pub pace: tokio::sync::mpsc::UnboundedSender<PaceCmd>,
    pub interrupt: tokio::sync::mpsc::UnboundedSender<crate::frontend::Interrupt>,
    pub sidebar: tokio::sync::mpsc::UnboundedSender<SidebarCmd>,
}

/// The blocking crossterm UI loop. Owns the terminal, the [`InteractiveState`],
/// and a crossterm event-reader thread; renders each frame from the projected
/// transcript. Adapted from ferritin's `ui_thread_loop` (the request-thread
/// plumbing is gone; inbound data is [`TuiEvent`]s from the bridge).
pub(super) fn ui_loop(
    events: Receiver<TuiEvent>,
    senders: Senders,
    theme_name: String,
    // `Some(label)` runs the loop as a read-only view of a recorded log
    // (docs/tui.md): no composer, no loop verbs, nothing that can send.
    viewing: Option<String>,
    // Time each paint and print the distribution on exit (`frame_cost.rs`). Off in
    // every normal run; the readout is not reproducible, so nothing that snapshots
    // can turn it on.
    frame_cost: bool,
) -> io::Result<()> {
    // Build the render context: interactive TTY, sized to the terminal.
    let width = crossterm::terminal::size()
        .map(|(w, _)| w as usize)
        .unwrap_or(80);
    let mut render_context = RenderContext::new()
        .with_interactive(true)
        .with_output_mode(OutputMode::Tty)
        .with_terminal_width(width);
    // `resolve_theme` already validated the name; a `.tmTheme` path could still
    // fail to load, so fall back to the default rather than abort the UI.
    if render_context.set_theme_name(&theme_name).is_err() {
        let _ = render_context.set_theme_name(DEFAULT_THEME);
    }
    let theme = InteractiveTheme::from_render_context(&render_context);

    // Leave immediate mode before a panic prints. Without this the default hook
    // writes the payload/backtrace straight into the alternate screen while raw
    // mode is still on — newlines don't return the carriage, so the trace lands
    // diagonally and is unreadable. We chain the original hook so the panic is
    // still reported, just onto a restored terminal. (Restored on clean exit by
    // the teardown at the end of this fn; the process exits right after either
    // way, so we don't bother re-installing the original hook.)
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = disable_raw_mode();
        let _ = execute!(
            stdout(),
            LeaveAlternateScreen,
            DisableMouseCapture,
            PopKeyboardEnhancementFlags,
            crossterm::cursor::Show
        );
        default_hook(info);
    }));

    enable_raw_mode()?;
    let mut stdout = stdout();
    // DISAMBIGUATE_ESCAPE_CODES lets kitty-protocol terminals report SUPER and
    // other unambiguous modifiers. Terminals that do not implement it continue
    // emitting their ordinary key events.
    execute!(
        stdout,
        EnterAlternateScreen,
        EnableMouseCapture,
        PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES)
    )?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;
    terminal.clear()?;

    let mut state = InteractiveState::new(
        events,
        senders.submit,
        senders.approve,
        senders.pace,
        senders.interrupt,
        senders.sidebar,
        render_context,
        theme,
    );
    if let Some(label) = viewing {
        state.set_viewing(label);
    }
    if frame_cost {
        state.frame_cost = Some(Default::default());
    }

    // Event reader thread: blocks on crossterm reads, forwards to a channel.
    let (event_tx, event_rx) = crossbeam_channel::unbounded();
    let _reader = thread::spawn(move || {
        while let Ok(evt) = event::read() {
            if event_tx.send(evt).is_err() {
                break;
            }
        }
    });

    let timer_tick = crossbeam_channel::tick(Duration::from_millis(30));
    // Elapsed-time displays (process running/idle seconds, seconds since the last
    // request) derive from absolute stamps and only change once a second — the wire
    // deliberately carries no ticking values (docs/control-api.md), so this tick is
    // what advances them.
    let clock_tick = crossbeam_channel::tick(Duration::from_secs(1));

    draw_measured(&mut terminal, &mut state)?;
    state.update_cursor(&mut terminal);

    let result = loop {
        // Default to *not* painting: daemon-side changes set `state.dirty` and ride
        // the redraw tick (debounce), so a reattach burst or a fast token stream
        // folds cheaply and paints at the frame rate rather than once per event.
        // Only user input and the tick request an immediate paint below.
        let mut redraw = false;
        let mut exit = false;
        select! {
            recv(state.events) -> ev => match ev {
                Ok(ev) => {
                    exit = state.handle_event(ev);
                    // Drain the rest of an arriving burst in one wakeup so it folds
                    // without interleaving projections — the crux of the O(N²)→O(N)
                    // reattach fix. Painting still waits for the next tick.
                    while !exit {
                        match state.events.try_recv() {
                            Ok(ev) => exit = state.handle_event(ev),
                            Err(_) => break,
                        }
                    }
                }
                Err(_) => exit = true, // bridge dropped → daemon gone.
            },
            recv(timer_tick) -> _ => {
                // Paint coalesced daemon changes and advance the streaming spinner.
                redraw = state.dirty || state.loading.pending_request;
            }
            recv(clock_tick) -> _ => {
                // Advance the elapsed-time displays; repaint only if one moved.
                redraw = state.refresh_clocks();
            }
            recv(event_rx) -> event => match event {
                // Apply the whole arriving burst, then paint once — the same fold
                // the daemon arm does above, for the same reason. A wheel notch or a
                // held key delivers many events between frames; giving each its own
                // full frame is what makes fast scrolling lag behind the wheel, since
                // the reader thread keeps queueing while the loop paints.
                Ok(first) => {
                    // A key may act on the transcript (expand/scroll), so realise any
                    // pending reproject before the burst is applied.
                    state.flush_reproject();
                    let mut next = Some(first);
                    while let Some(event) = next.take() {
                        match event {
                            Event::Key(key) if !key.is_release() => {
                                exit = state.handle_key_event(key, &mut terminal);
                                redraw = true;
                            }
                            Event::Mouse(mouse_event) => {
                                state.handle_mouse_event(mouse_event, &terminal);
                                redraw = true;
                            }
                            Event::Resize(_, _) => {
                                // The terminal changed size: the cached layout (wrap +
                                // document height) is width-dependent, so drop it and
                                // repaint now. Without this the resize only takes on the
                                // *next* unrelated event (a mouse re-enter), which read as
                                // "resize does nothing until I wiggle the mouse." The
                                // sidebar dock threshold makes this especially visible.
                                state.viewport.cached_layout = None;
                                redraw = true;
                            }
                            _ => {}
                        }
                        // Stop the fold where an event can't be folded: a quit, or a
                        // click, which the paint below has to consume before another
                        // can be recorded in its place.
                        if exit
                            || state.ui.quit_requested
                            || state.viewport.clicked_position.is_some()
                        {
                            break;
                        }
                        next = event_rx.try_recv().ok();
                    }
                }
                Err(_) => exit = true,
            },
        }

        if exit || state.ui.quit_requested {
            break Ok(());
        }
        if !redraw {
            continue;
        }

        // Realise any pending reproject, then clear the dirty flag for this paint.
        state.flush_reproject();
        state.dirty = false;
        state.handle_hover();
        state.handle_click();
        // One draw per frame: `render_frame` measures the document and re-pins a
        // stream-following viewport to the bottom before it paints (docs/tui.md),
        // so there is no stale-offset paint to correct afterwards.
        draw_measured(&mut terminal, &mut state)?;

        state.update_cursor(&mut terminal);
    };

    // Restore the terminal.
    disable_raw_mode()?;
    if state.ui.supports_cursor {
        set_cursor_shape(terminal.backend_mut(), "default");
    }
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture,
        PopKeyboardEnhancementFlags
    )?;
    terminal.show_cursor()?;

    // After the alt-screen is gone, so the distribution lands on the real terminal
    // rather than being wiped with the scrollback (`frame_cost.rs`).
    if let Some(summary) = state.frame_cost.as_ref().and_then(FrameCost::summary) {
        eprintln!("{summary}");
    }
    result
}

/// One paint, timed when `--frame-cost` asked for it. The measurement wraps the
/// whole `draw` — the document measure pass and the paint are one unit of work, and
/// splitting them would attribute layout cost to whichever half ran second.
fn draw_measured(
    terminal: &mut Terminal<impl Backend<Error = io::Error> + Write>,
    state: &mut InteractiveState<'_>,
) -> io::Result<()> {
    if state.frame_cost.is_none() {
        terminal.draw(|frame| state.render_frame(frame))?;
        return Ok(());
    }
    let started = std::time::Instant::now();
    terminal.draw(|frame| state.render_frame(frame))?;
    let micros = started.elapsed().as_micros().min(u32::MAX as u128) as u32;
    let (laid_out, highlighted) = (state.document.laid_out, state.document.highlighted);
    if let Some(cost) = state.frame_cost.as_mut() {
        cost.record(micros, laid_out, highlighted);
    }
    Ok(())
}
