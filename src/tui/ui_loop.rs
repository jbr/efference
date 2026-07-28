//! The blocking crossterm UI loop: owns the terminal and renders each frame.

use super::*;

/// The blocking crossterm UI loop. Owns the terminal, the [`InteractiveState`],
/// and a crossterm event-reader thread; renders each frame from the projected
/// transcript. Adapted from ferritin's `ui_thread_loop` (the request-thread
/// plumbing is gone; inbound data is [`TuiEvent`]s from the bridge).
pub(super) fn ui_loop(
    events: Receiver<TuiEvent>,
    submit_tx: tokio::sync::mpsc::UnboundedSender<String>,
    approve_tx: tokio::sync::mpsc::UnboundedSender<(String, crate::frontend::Approval)>,
    pace_tx: tokio::sync::mpsc::UnboundedSender<PaceCmd>,
    interrupt_tx: tokio::sync::mpsc::UnboundedSender<crate::frontend::Interrupt>,
    theme_name: String,
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
            crossterm::cursor::Show
        );
        default_hook(info);
    }));

    enable_raw_mode()?;
    let mut stdout = stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;
    terminal.clear()?;

    let mut state = InteractiveState::new(
        events,
        submit_tx,
        approve_tx,
        pace_tx,
        interrupt_tx,
        render_context,
        theme,
    );

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

    terminal.draw(|frame| state.render_frame(frame))?;
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
            recv(event_rx) -> event => match event {
                Ok(Event::Key(key)) if !key.is_release() => {
                    // A key may act on the transcript (expand/scroll), so realise any
                    // pending reproject first; then paint immediately for responsiveness.
                    state.flush_reproject();
                    exit = state.handle_key_event(key, &mut terminal);
                    redraw = true;
                }
                Ok(Event::Mouse(mouse_event)) => {
                    state.flush_reproject();
                    state.handle_mouse_event(mouse_event, &terminal);
                    redraw = true;
                }
                Ok(Event::Resize(_, _)) => {
                    // The terminal changed size: the cached layout (wrap + document
                    // height) is width-dependent, so drop it and repaint now. Without
                    // this the resize only takes on the *next* unrelated event (a mouse
                    // re-enter), which read as "resize does nothing until I wiggle the
                    // mouse." The sidebar dock threshold makes this especially visible.
                    state.viewport.cached_layout = None;
                    redraw = true;
                }
                Ok(_) => {}
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
        terminal.draw(|frame| state.render_frame(frame))?;

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
        DisableMouseCapture
    )?;
    terminal.show_cursor()?;
    result
}
