//! The blocking crossterm UI loop: owns the terminal and renders each frame.

use super::*;

use crate::tui::frame_cost::FrameCost;
use ratatui::prelude::Backend;
use std::io::Write;

/// The blocking crossterm UI loop. Owns the terminal, the [`InteractiveState`],
/// and a crossterm event-reader thread; renders each frame from the projected
/// transcript. Adapted from ferritin's `ui_thread_loop` (the request-thread
/// plumbing is gone; inbound data is [`TuiEvent`]s from the bridge).
pub(super) fn ui_loop(
    events: Receiver<TuiEvent>,
    senders: Senders,
    theme_name: String,
    // How to pick the graphics protocol for result images (docs/tui.md).
    images: ImageMode,
    // `Some(label)` runs the loop as a read-only view of a recorded log
    // (docs/tui.md): no composer, no loop verbs, nothing that can send.
    viewing: Option<String>,
    // Time each paint and print the distribution on exit (`frame_cost.rs`). Off in
    // every normal run; the readout is not reproducible, so nothing that snapshots
    // can turn it on.
    frame_cost: bool,
    // The default for a run with no session behind it — the gallery and the
    // read-only view.
    attached: super::run::Attached,
) -> io::Result<Exit> {
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

    let mut state = InteractiveState::new(events, senders, render_context, theme);
    state.attachments_dir = attached.attachments_dir;
    state.owns_host = attached.owns_host;
    if let Some(label) = viewing {
        state.set_viewing(label);
    }
    if frame_cost {
        state.frame_cost = Some(Default::default());
    }

    // Detect the graphics protocol **before** the reader thread spawns: the
    // query writes escapes and reads their answers off stdin, which the reader
    // is about to own for the session (docs/tui.md — detection is a startup
    // act; capability is then a plain parameter of rendering).
    let picker = build_picker(images);
    if let Some(picker) = &picker {
        // Name the resolved protocol on the status line at launch — detection
        // is invisible otherwise, and "which protocol did auto pick" is the
        // first question when an image renders wrong (docs/tui.md).
        let font = picker.font_size();
        state.set_debug_message(format!(
            "images: {:?} · font {}×{} — ↑ controls",
            picker.protocol_type(),
            font.width,
            font.height,
        ));
    }
    state.images.set_picker(picker);
    // Aspect-fit padding takes the theme's background so it disappears into
    // the transcript (iTerm2 renders the default transparent pad as opaque).
    state
        .images
        .set_theme_background(state.theme.document_bg_style.bg);

    // Event reader thread: polls crossterm, forwards to a channel. It polls with a
    // short timeout rather than blocking in `read`, and is stopped and joined when
    // the loop ends, because crossterm serializes every stdin read — an event read
    // and a terminal query's answer alike — behind one lock that a blocked `read`
    // holds. A reader left blocked from this attach would starve the next one's
    // startup queries (the cursor-position read times out), which is what a
    // session switch or a followed hand-off does next (docs/tui.md).
    let (event_tx, event_rx) = crossbeam_channel::unbounded();
    let reading = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
    let reader = {
        let reading = std::sync::Arc::clone(&reading);
        thread::spawn(move || {
            while reading.load(std::sync::atomic::Ordering::Relaxed) {
                match event::poll(Duration::from_millis(50)) {
                    Ok(true) => match event::read() {
                        Ok(evt) => {
                            if event_tx.send(evt).is_err() {
                                break;
                            }
                        }
                        Err(_) => break,
                    },
                    Ok(false) => {}
                    Err(_) => break,
                }
            }
        })
    };

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
            // The switcher is the only thing that sets a destination; every
            // other way out of the loop is a plain quit (`sessions.rs`).
            break Ok(state.switch_to.take().unwrap_or(Exit::Quit));
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

    // Release stdin before anything else touches the terminal: the next attach's
    // queries need the lock the reader polls under.
    reading.store(false, std::sync::atomic::Ordering::Relaxed);
    let _ = reader.join();

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

/// The terminal's graphics capability per the operator's [`ImageMode`]
/// (docs/tui.md). `Auto` trusts the in-band query — kitty's query-plus-DA1
/// fence, iTerm2's env identification — and falls back to halfblocks, which any
/// color terminal can show. A forced protocol still runs the query for the
/// font-size-in-pixels answer (it maps image pixels to cells); only when that
/// fails too does a stock 8×16 stand in.
fn build_picker(mode: ImageMode) -> Option<ratatui_image::picker::Picker> {
    use ratatui_image::picker::{Picker, ProtocolType};
    let forced = match mode {
        ImageMode::Off => return None,
        ImageMode::Auto => None,
        ImageMode::Kitty => Some(ProtocolType::Kitty),
        ImageMode::Iterm2 => Some(ProtocolType::Iterm2),
    };
    // The fallback carries `halfblocks()`'s assumed 10×20 font; a forced
    // protocol overrides the protocol choice but keeps whichever font answer
    // the query managed.
    let mut picker = Picker::from_query_stdio().unwrap_or_else(|_| Picker::halfblocks());
    if let Some(protocol) = forced {
        picker.set_protocol_type(protocol);
    } else if identifies_as_iterm2() {
        // iTerm2 answers the capability queries above its own protocol's weight
        // class — DA1 advertises sixel, and 3.5+ answers the kitty query with a
        // partial implementation — so query-ranked detection lands on a protocol
        // iTerm2 then renders as garbage (sixel control characters print as red
        // text). OSC 1337 is its native, 24-bit protocol; on auto, an
        // iTerm2-identified terminal always gets it.
        picker.set_protocol_type(ProtocolType::Iterm2);
    }
    Some(picker)
}

/// Whether the environment identifies the terminal as iTerm2 — the same
/// signals the crate's own env fallback reads (`TERM_PROGRAM`, and
/// `LC_TERMINAL`, which iTerm2 forwards over ssh).
fn identifies_as_iterm2() -> bool {
    [std::env::var("TERM_PROGRAM"), std::env::var("LC_TERMINAL")]
        .iter()
        .any(|v| matches!(v.as_deref(), Ok(term) if term.contains("iTerm")))
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
