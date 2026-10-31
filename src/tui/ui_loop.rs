//! The blocking crossterm UI loop: owns the terminal and renders each frame.

use super::*;

use crate::terminal_title::{self, TerminalTitle};
use crate::tui::frame_cost::FrameCost;
use crate::user_config::{ImageMode, TuiConfig};
use ratatui::prelude::Backend;
use std::io::Write;

/// The blocking crossterm UI loop. Owns the terminal, the [`InteractiveState`],
/// and a crossterm event-reader thread; renders each frame from the projected
/// transcript. Adapted from ferritin's `ui_thread_loop` (the request-thread
/// plumbing is gone; inbound data is [`TuiEvent`]s from the bridge).
pub(super) fn ui_loop(
    events: Receiver<TuiEvent>,
    senders: Senders,
    // How the UI draws (docs/tui.md): its theme, already resolved by `resolve_theme`; how
    // to pick the graphics protocol for result images; and `frame_cost`, which times each
    // paint and prints the distribution on exit (`frame_cost.rs`). The timing is off in
    // every normal run; the readout is not reproducible, so nothing that snapshots can
    // turn it on.
    display: &TuiConfig,
    // `Some(label)` runs the loop as a read-only view of a recorded log
    // (docs/tui.md): no composer, no loop verbs, nothing that can send.
    viewing: Option<String>,
    // The default for a run with no session behind it — the gallery and the
    // read-only view.
    attached: &Attached,
    // Reports whether the operator is attending (`attention.rs`); `None` where no
    // host is behind the run.
    mut attender: Option<attention::Attender>,
) -> io::Result<Exit> {
    let attention_tx = senders.attention.clone();
    let send_report = |report: Option<crate::protocol::attention::AttentionReport>,
                       attender: &mut Option<attention::Attender>| {
        if let Some(report) = report
            && attention_tx.send(report).is_err()
        {
            // The dispatcher ended: the host serves no attention table here.
            if let Some(attender) = attender {
                attender.stop();
            }
        }
    };
    let (render_context, theme) = render_context_for(display);
    install_panic_hook();

    let mut terminal = enter_terminal()?;
    let mut title = TerminalTitle::default();
    let mut state = build_state(
        events,
        senders,
        render_context,
        theme,
        display,
        attached,
        viewing,
    );
    let (event_rx, reader) = spawn_event_reader();

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
                    // The identity arrives only on this channel; `show` skips an
                    // unchanged title.
                    if let Some(identity) = &state.topics.session_identity {
                        let text = terminal_title::for_session(
                            &identity.id,
                            identity.title.as_deref(),
                        );
                        title.show(terminal.backend_mut(), &text)?;
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
                Ok(first) => {
                    (exit, redraw) =
                        fold_terminal_events(first, &mut state, &mut terminal, &mut attender, &event_rx);
                }
                Err(_) => exit = true,
            },
        }

        let report = attender
            .as_mut()
            .and_then(|a| a.due(std::time::Instant::now()));
        send_report(report, &mut attender);

        if exit || state.ui.quit_requested {
            // The switcher and a closed stream are the only things that set a
            // destination; every other way out of the loop is a plain quit
            // (`sessions.rs`).
            let exit = state.switch_to.take().unwrap_or(Exit::Quit);
            // Quitting says so at once; a switch leaves it to the next attach's
            // first report, which would otherwise race this one.
            if matches!(exit, Exit::Quit | Exit::Closed) {
                let report = attender.as_mut().and_then(attention::Attender::leaving);
                send_report(report, &mut attender);
            }
            break Ok(exit);
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

    reader.stop();
    leave_terminal(&mut terminal, &state)?;
    result
}

/// The render context for an interactive TTY sized to the terminal, and the theme drawn
/// from it.
fn render_context_for(display: &TuiConfig) -> (RenderContext, InteractiveTheme) {
    let width = crossterm::terminal::size().map_or(80, |(w, _)| w as usize);
    let mut render_context = RenderContext::new()
        .with_interactive(true)
        .with_output_mode(OutputMode::Tty)
        .with_terminal_width(width);
    // `resolve_theme` already validated the name; a `.tmTheme` path could still
    // fail to load, so fall back to the default rather than abort the UI.
    let theme_name = display.theme.as_deref().unwrap_or(DEFAULT_THEME);
    if render_context.set_theme_name(theme_name).is_err() {
        let _ = render_context.set_theme_name(DEFAULT_THEME);
    }
    let theme = InteractiveTheme::from_render_context(&render_context);
    (render_context, theme)
}

/// Leave immediate mode before a panic prints. Without this the default hook
/// writes the payload/backtrace straight into the alternate screen while raw
/// mode is still on — newlines don't return the carriage, so the trace lands
/// diagonally and is unreadable. We chain the original hook so the panic is
/// still reported, just onto a restored terminal. (Restored on clean exit by
/// [`leave_terminal`]; the process exits right after either way, so we don't
/// bother re-installing the original hook.)
fn install_panic_hook() {
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = disable_raw_mode();
        let _ = execute!(
            stdout(),
            LeaveAlternateScreen,
            DisableMouseCapture,
            DisableBracketedPaste,
            DisableFocusChange,
            PopKeyboardEnhancementFlags,
            crossterm::cursor::Show
        );
        let _ = terminal_title::restore(&mut stdout());
        default_hook(info);
    }));
}

/// Take the terminal over: raw mode, the alternate screen, and the input modes the UI reads.
fn enter_terminal() -> io::Result<Terminal<CrosstermBackend<io::Stdout>>> {
    enable_raw_mode()?;
    let mut stdout = stdout();
    // DISAMBIGUATE_ESCAPE_CODES lets kitty-protocol terminals report SUPER and
    // other unambiguous modifiers. Terminals that do not implement it continue
    // emitting their ordinary key events. Bracketed paste delivers a paste (⌘V,
    // a file dragged onto the window) as one event rather than a burst of typed
    // keys, which is what lets it be routed to an attachment (docs/tui.md).
    execute!(
        stdout,
        EnterAlternateScreen,
        EnableMouseCapture,
        EnableBracketedPaste,
        EnableFocusChange,
        PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES)
    )?;
    // The terminal title names the attached session once its identity arrives
    // (docs/tui.md); the one in place now comes back at teardown.
    terminal_title::save(&mut stdout)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;
    terminal.clear()?;
    Ok(terminal)
}

/// The interactive state for this run: the connected channels, the viewing mode, and the
/// graphics capability the images are drawn with.
fn build_state<'a>(
    events: Receiver<TuiEvent>,
    senders: Senders,
    render_context: RenderContext,
    theme: InteractiveTheme,
    display: &TuiConfig,
    attached: &Attached,
    viewing: Option<String>,
) -> InteractiveState<'a> {
    let mut state = InteractiveState::new(events, senders, render_context, theme);
    state.owns_host = attached.owns_host;
    if let Some(draft) = &attached.draft {
        state.composer.set_text(draft);
    }
    if let Some(label) = viewing {
        state.set_viewing(label);
    }
    if display.frame_cost {
        state.frame_cost = Some(Default::default());
    }

    // Detect the graphics protocol **before** the reader thread spawns: the
    // query writes escapes and reads their answers off stdin, which the reader
    // is about to own for the session (docs/tui.md — detection is a startup
    // act; capability is then a plain parameter of rendering).
    let picker = build_picker(display.images);
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
    state.staged_images.set_picker(picker.clone());
    state.images.set_picker(picker);
    // Aspect-fit padding takes the theme's background so it disappears into
    // the transcript (iTerm2 renders the default transparent pad as opaque).
    let background = state.theme.document_bg_style.bg;
    state.images.set_theme_background(background);
    state.staged_images.set_theme_background(background);
    state
}

/// The crossterm event-reader thread: polls crossterm and forwards to a channel. It polls
/// with a short timeout rather than blocking in `read`, and is stopped and joined when
/// the loop ends, because crossterm serializes every stdin read — an event read
/// and a terminal query's answer alike — behind one lock that a blocked `read`
/// holds. A reader left blocked from this attach would starve the next one's
/// startup queries (the cursor-position read times out), which is what a
/// session switch or a followed hand-off does next (docs/tui.md).
struct EventReader {
    reading: std::sync::Arc<std::sync::atomic::AtomicBool>,
    thread: thread::JoinHandle<()>,
}

fn spawn_event_reader() -> (Receiver<Event>, EventReader) {
    let (event_tx, event_rx) = crossbeam_channel::unbounded();
    let reading = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
    let thread = {
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
    (event_rx, EventReader { reading, thread })
}

impl EventReader {
    /// Release stdin before anything else touches the terminal: the next attach's
    /// queries need the lock the reader polls under.
    fn stop(self) {
        self.reading
            .store(false, std::sync::atomic::Ordering::Relaxed);
        let _ = self.thread.join();
    }
}

/// Apply the whole arriving burst of terminal events, then let the caller paint once — the
/// same fold the daemon arm does, for the same reason. A wheel notch or a held key delivers
/// many events between frames; giving each its own full frame is what makes fast scrolling
/// lag behind the wheel, since the reader thread keeps queueing while the loop paints.
/// Returns whether the operator left, and whether to paint.
fn fold_terminal_events(
    first: Event,
    state: &mut InteractiveState<'_>,
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    attender: &mut Option<attention::Attender>,
    event_rx: &Receiver<Event>,
) -> (bool, bool) {
    let (mut exit, mut redraw) = (false, false);
    // A key may act on the transcript (expand/scroll), so realise any
    // pending reproject before the burst is applied.
    state.flush_reproject();
    let mut next = Some(first);
    while let Some(event) = next.take() {
        // Focus and input feed the attention reports (`attention.rs`).
        if let Some(attender) = attender.as_mut() {
            match &event {
                Event::FocusGained => attender.focus(true),
                Event::FocusLost => attender.focus(false),
                Event::Key(_) | Event::Mouse(_) => {
                    attender.input(std::time::Instant::now());
                }
                _ => {}
            }
        }
        match event {
            Event::Key(key) if !key.is_release() => {
                exit = state.handle_key_event(key, terminal);
                redraw = true;
            }
            Event::Mouse(mouse_event) => {
                state.handle_mouse_event(mouse_event, terminal);
                redraw = true;
            }
            Event::Paste(text) => {
                state.handle_paste(&text);
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
        if exit || state.ui.quit_requested || state.viewport.clicked_position.is_some() {
            break;
        }
        next = event_rx.try_recv().ok();
    }
    (exit, redraw)
}

/// Put the terminal back as it was found.
fn leave_terminal(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    state: &InteractiveState<'_>,
) -> io::Result<()> {
    disable_raw_mode()?;
    if state.ui.supports_cursor {
        set_cursor_shape(terminal.backend_mut(), "default");
    }
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture,
        DisableBracketedPaste,
        DisableFocusChange,
        PopKeyboardEnhancementFlags
    )?;
    terminal_title::restore(terminal.backend_mut())?;
    terminal.show_cursor()?;

    // After the alt-screen is gone, so the distribution lands on the real terminal
    // rather than being wiped with the scrollback (`frame_cost.rs`).
    if let Some(summary) = state.frame_cost.as_ref().and_then(FrameCost::summary) {
        eprintln!("{summary}");
    }
    Ok(())
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

/// One paint, timed when `frame-cost` under `[tui]` (or `dev gallery --frame-cost`)
/// asked for it. The measurement wraps the whole `draw` — the document measure pass
/// and the paint are one unit of work, and splitting them would attribute layout
/// cost to whichever half ran second.
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
