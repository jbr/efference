//! The interactive TUI (DESIGN §10 — the human-facing client).
//!
//! Vendored-and-adapted from ferritin's interactive renderer. The load-bearing
//! seam ferritin exposed — a general-purpose document IR ([`document`]) rendered
//! by a domain-free pipeline ([`render_node`] et al.) over a syntect theme
//! ([`theme`], [`render_context`]) — carries over almost verbatim. What changed:
//!
//! - The rustdoc shell (Navigator/Request/Commands and the two-thread request
//!   architecture) is gone; the efference daemon *is* the request side. The TUI
//!   subscribes to the daemon's `/events` + `/stream` SSE and projects the log
//!   into a [`document::Document`] via [`projection`], re-projected as events land.
//! - The IR's navigation appendage (rustdoc `Navigate` actions, `Metadata` item
//!   blocks tied to `DocRef`) is stripped; what remains is markdown + code +
//!   collapsible blocks.

// Vendored from ferritin: the IR and theme modules keep a full constructor /
// helper surface (Span builders, node convenience ctors, …) that efference does
// not exercise yet but that keeps the code shaped like upstream for backporting.
#![allow(dead_code)]

pub(crate) mod color_scheme;
pub(crate) mod composer;
pub(crate) mod gallery;
#[cfg(test)]
pub(crate) mod harness;
pub(crate) mod document;
pub(crate) mod events;
pub(crate) mod keyboard;
pub(crate) mod markdown;
pub(crate) mod mouse;
pub(crate) mod projection;
pub(crate) mod render_code_block;
pub(crate) mod render_context;
pub(crate) mod render_document;
pub(crate) mod render_frame;
pub(crate) mod render_help_screen;
pub(crate) mod render_loading_bar;
pub(crate) mod render_node;
pub(crate) mod render_scrollbar;
pub(crate) mod render_span;
pub(crate) mod render_status_bar;
pub(crate) mod render_table;
pub(crate) mod render_theme_picker;
pub(crate) mod span_style;
pub(crate) mod state;
pub(crate) mod table_layout;
pub(crate) mod theme;
pub(crate) mod utils;
pub(crate) mod write_text;

// The render pipeline and input handlers reference these via `super::` (i.e. the
// `tui` module namespace), mirroring ferritin's `renderer::interactive` layout.
// Private re-export: visible to the `tui` submodules, not the wider crate.
use state::{InteractiveState, UiMode};

/// One inbound message to the UI thread, bridged from the daemon's SSE streams
/// by the tokio side (see `main.rs`). Durable [`Entry`](TuiEvent::Entry) values
/// come from `/events` (a serialized [`LogEntry`](crate::log::LogEntry)); the
/// ephemeral deltas come from `/stream` and are folded into the live tail until
/// their committed entry lands.
#[derive(Debug)]
pub(crate) enum TuiEvent {
    /// A committed log entry (from the durable `/events` tail).
    Entry(crate::log::LogEntry),
    /// A fragment of in-progress assistant prose (`/stream` `assistant_delta`).
    AssistantDelta(String),
    /// A fragment of a running tool's output (`/stream` `tool_output`).
    ToolOutput { call_id: String, chunk: String },
    /// The pending tool-approval gate changed (polled from `GET /state`, since the
    /// gate is ephemeral control state with no SSE event — DESIGN §10.0 parks
    /// "pending-approval as synthetic `/events`" for later).
    Gate(Option<PendingGate>),
    /// The live background processes changed (polled from `GET /state` alongside the
    /// gate, DESIGN §6 — the running tail is ephemeral, never logged, so it has no
    /// `/events` entry; the process panel is fed by the poll). TUI-bg-2 will layer
    /// smooth `/stream` `bg_output` deltas on top.
    Processes(Vec<BgProc>),
    /// The event source closed (daemon gone) → the UI should exit.
    Disconnected,
}

/// One live background process, as surfaced by `GET /state`'s `processes` array
/// (the serialized `supervisor::BgView`, DESIGN §6). `Serialize` too so the poller
/// can fingerprint the list and only emit on change.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub(crate) struct BgProc {
    pub handle: String,
    pub command: String,
    pub tail: String,
    pub running_secs: u64,
    pub idle_secs: u64,
}

/// A pending tool-approval gate, as surfaced by `GET /state`'s `pending_approval`.
#[derive(Debug, Clone)]
pub(crate) struct PendingGate {
    pub call_id: String,
    pub name: String,
    pub input: serde_json::Value,
}

use std::io::{self, stdout};
use std::thread;
use std::time::Duration;

use anyhow::{Context, Result};
use crossbeam_channel::{Receiver, Sender, select};
use crossterm::{
    event::{self, DisableMouseCapture, EnableMouseCapture, Event},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use futures_lite::StreamExt;
use ratatui::{Terminal, backend::CrosstermBackend};

use crate::client::Client;
use crate::log::LogEntry;
use render_context::{OutputMode, RenderContext};
use theme::InteractiveTheme;
use utils::set_cursor_shape;

/// Run the interactive TUI against a daemon control socket.
///
/// Subscribes to the daemon's `/events` (durable log) and `/stream` (ephemeral
/// deltas) over the [`Client`], bridges them into the UI thread, and renders the
/// transcript until the user quits or the daemon goes away. The crossterm event
/// loop blocks, so the UI runs on a dedicated blocking thread while the SSE
/// bridge runs as a tokio task (DESIGN §10 — the TUI is one client of the socket).
/// The default theme — nicer than `THEME_NAMES[0]` ("1337", which only wins by
/// sort order). Overridable with `--theme` / `EFFERENCE_THEME`.
const DEFAULT_THEME: &str = "Catppuccin Frappe";

pub async fn run(client: Client, theme: Option<String>) -> Result<()> {
    use crate::frontend::Approval;

    // Validate the theme up front, before the alt-screen takes over, so a bad
    // name surfaces a clean error (with the list) instead of a silent fallback.
    let theme = resolve_theme(theme)?;

    let (tx, rx) = crossbeam_channel::unbounded::<TuiEvent>();
    let (submit_tx, mut submit_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    let (approve_tx, mut approve_rx) =
        tokio::sync::mpsc::unbounded_channel::<(String, Approval)>();

    // The bridge feeds the UI thread; it ends when the streams close or the UI
    // thread drops the receiver.
    let bridge = tokio::spawn(bridge(client.clone(), tx.clone()));

    // Gate poller: the approval gate is ephemeral control state (`GET /state`) with
    // no SSE event, so poll it and emit a `Gate` only when it changes (M3).
    let poll_client = client.clone();
    let gate_tx = tx.clone();
    let poller = tokio::spawn(async move {
        let mut last: Option<String> = None;
        let mut last_procs = String::new();
        loop {
            tokio::time::sleep(std::time::Duration::from_millis(400)).await;
            let Ok(body) = poll_client.state().await else {
                continue;
            };
            let gate = parse_pending_gate(&body);
            let cur = gate.as_ref().map(|g| g.call_id.clone());
            if cur != last {
                last = cur;
                if gate_tx.send(TuiEvent::Gate(gate)).is_err() {
                    break;
                }
            }
            // The live process list rides the same poll (DESIGN §6). Emit only when it
            // changes — but `running`/`idle` secs tick, so a running process refreshes
            // ~1Hz, which is exactly the live-ish cadence the panel wants.
            let procs = parse_processes(&body);
            let fp = serde_json::to_string(&procs).unwrap_or_default();
            if fp != last_procs {
                last_procs = fp;
                if gate_tx.send(TuiEvent::Processes(procs)).is_err() {
                    break;
                }
            }
        }
    });

    // Turn sender: each composer submission is POSTed as a user turn (M2 write path).
    let turn_client = client.clone();
    let sender = tokio::spawn(async move {
        while let Some(text) = submit_rx.recv().await {
            let _ = turn_client.turn(&text).await;
        }
    });

    // Approval sender: resolve a gate via `POST /approvals/:call_id` (M3).
    let approve_client = client.clone();
    let approver = tokio::spawn(async move {
        while let Some((call_id, decision)) = approve_rx.recv().await {
            let _ = approve_client.approve(&call_id, &decision).await;
        }
    });

    // The crossterm UI loop is blocking — run it off the async runtime.
    let ui = tokio::task::spawn_blocking(move || ui_loop(rx, submit_tx, approve_tx, theme));

    let result = ui.await.context("the TUI thread panicked")?;
    bridge.abort();
    poller.abort();
    sender.abort();
    approver.abort();
    result.context("the TUI event loop failed")
}

/// Launch the TUI on the fixed [`gallery`] transcript — every element type, no
/// daemon. The events channel is pre-seeded and its sender held for the session's
/// life so the UI never sees a `Disconnected` (which would exit immediately); the
/// submit/approve far-ends are likewise held so the composer doesn't error. Real
/// `ui_loop`, real rendering — just a synthetic, offline log to look at.
pub async fn run_gallery(theme: Option<String>) -> Result<()> {
    let theme = resolve_theme(theme)?;

    let (tx, rx) = crossbeam_channel::unbounded::<TuiEvent>();
    let (submit_tx, submit_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    let (approve_tx, approve_rx) =
        tokio::sync::mpsc::unbounded_channel::<(String, crate::frontend::Approval)>();

    for entry in gallery::log() {
        let _ = tx.send(TuiEvent::Entry(entry));
    }

    let ui = tokio::task::spawn_blocking(move || ui_loop(rx, submit_tx, approve_tx, theme));
    let result = ui.await.context("the TUI thread panicked")?;

    // Hold the far-ends until the UI exits, so the channels stay open for its life.
    drop(tx);
    drop(submit_rx);
    drop(approve_rx);

    result.context("the TUI event loop failed")
}

/// Resolve the theme to apply. An explicit name must exist in the built-in set
/// (or be a `.tmTheme` path) — an unknown one is a hard error listing the
/// options. With no override, prefer [`DEFAULT_THEME`], falling back to whatever
/// sorts first if the theme set doesn't include it (e.g. submodules not fetched).
fn resolve_theme(theme: Option<String>) -> Result<String> {
    let available = RenderContext::available_themes();
    match theme {
        Some(name) => {
            if name.ends_with(".tmTheme") || available.iter().any(|t| t == &name) {
                Ok(name)
            } else {
                anyhow::bail!(
                    "unknown theme {name:?}.\nAvailable themes: {}",
                    available.join(", ")
                )
            }
        }
        None if available.iter().any(|t| t == DEFAULT_THEME) => Ok(DEFAULT_THEME.to_string()),
        None => Ok(available
            .into_iter()
            .next()
            .unwrap_or_else(|| DEFAULT_THEME.to_string())),
    }
}

#[cfg(test)]
mod theme_tests {
    use super::{DEFAULT_THEME, resolve_theme};

    #[test]
    fn default_prefers_catppuccin_frappe() {
        // The submodule theme set includes it, so the no-override default is it
        // (not the sort-first "1337").
        assert_eq!(resolve_theme(None).unwrap(), DEFAULT_THEME);
    }

    #[test]
    fn a_known_name_passes_through() {
        assert_eq!(resolve_theme(Some("Nord".into())).unwrap(), "Nord");
    }

    #[test]
    fn an_unknown_name_is_an_error() {
        assert!(resolve_theme(Some("definitely-not-a-theme".into())).is_err());
    }

    #[test]
    fn a_tmtheme_path_is_accepted_without_membership() {
        let p = "/tmp/Custom.tmTheme";
        assert_eq!(resolve_theme(Some(p.into())).unwrap(), p);
    }
}

#[cfg(test)]
mod gate_tests {
    use super::parse_pending_gate;

    #[test]
    fn parses_a_pending_gate() {
        let body = r#"{"last_seq":1,"pending_approval":{"call_id":"toolu_x","input":{"command":"ls"},"name":"bash"},"usage":{}}"#;
        let gate = parse_pending_gate(body).expect("a gate");
        assert_eq!(gate.call_id, "toolu_x");
        assert_eq!(gate.name, "bash");
        assert_eq!(gate.input["command"], "ls");
    }

    #[test]
    fn null_pending_approval_is_no_gate() {
        let body = r#"{"last_seq":7,"pending_approval":null,"usage":{}}"#;
        assert!(parse_pending_gate(body).is_none());
    }
}

/// Parse `GET /state`'s `processes` array into the live [`BgProc`] list (DESIGN §6).
/// A missing/malformed field is an empty list — the panel just hides.
fn parse_processes(body: &str) -> Vec<BgProc> {
    serde_json::from_str::<serde_json::Value>(body)
        .ok()
        .and_then(|v| v.get("processes").cloned())
        .and_then(|p| serde_json::from_value::<Vec<BgProc>>(p).ok())
        .unwrap_or_default()
}

/// Parse `GET /state`'s `pending_approval` object into a [`PendingGate`].
fn parse_pending_gate(body: &str) -> Option<PendingGate> {
    let value: serde_json::Value = serde_json::from_str(body).ok()?;
    let pending = value.get("pending_approval")?;
    if pending.is_null() {
        return None;
    }
    Some(PendingGate {
        call_id: pending.get("call_id")?.as_str()?.to_string(),
        name: pending.get("name")?.as_str()?.to_string(),
        input: pending.get("input").cloned().unwrap_or(serde_json::Value::Null),
    })
}

/// Decode the daemon's two SSE streams into [`TuiEvent`]s. Mirrors the decode in
/// `main.rs`'s `render_streams`, but emits typed events instead of printing.
async fn bridge(client: Client, tx: Sender<TuiEvent>) -> Result<()> {
    let mut events = client.events(Some(0)).await?;
    let mut deltas = client.stream().await?;
    let mut deltas_done = false;

    loop {
        tokio::select! {
            event = events.next() => match event {
                Some(Ok(event)) => {
                    if event.event_type() != Some("keepalive")
                        && let Ok(entry) = serde_json::from_str::<LogEntry>(event.data())
                        && tx.send(TuiEvent::Entry(entry)).is_err()
                    {
                        break; // UI thread gone.
                    }
                }
                // Stream error or end: the durable tail is authoritative, so its
                // loss means the daemon is gone.
                _ => {
                    let _ = tx.send(TuiEvent::Disconnected);
                    break;
                }
            },
            delta = deltas.next(), if !deltas_done => match delta {
                Some(Ok(delta)) => {
                    if decode_delta(&delta, &tx).is_break() {
                        break;
                    }
                }
                Some(Err(_)) | None => deltas_done = true,
            },
        }
    }
    Ok(())
}

/// Decode one ephemeral delta event, sending the matching [`TuiEvent`].
fn decode_delta(event: &trillium_client::sse::Event, tx: &Sender<TuiEvent>) -> std::ops::ControlFlow<()> {
    let value: serde_json::Value = serde_json::from_str(event.data()).unwrap_or_default();
    let sent = match event.event_type() {
        Some("assistant_delta") => value["text"]
            .as_str()
            .map(|text| tx.send(TuiEvent::AssistantDelta(text.to_string()))),
        Some("tool_output") => match (value["call_id"].as_str(), value["chunk"].as_str()) {
            (Some(call_id), Some(chunk)) => Some(tx.send(TuiEvent::ToolOutput {
                call_id: call_id.to_string(),
                chunk: chunk.to_string(),
            })),
            _ => None,
        },
        _ => None,
    };
    // `Some(Err(..))` means the UI receiver was dropped → stop.
    if matches!(sent, Some(Err(_))) {
        std::ops::ControlFlow::Break(())
    } else {
        std::ops::ControlFlow::Continue(())
    }
}

/// The blocking crossterm UI loop. Owns the terminal, the [`InteractiveState`],
/// and a crossterm event-reader thread; renders each frame from the projected
/// transcript. Adapted from ferritin's `ui_thread_loop` (the request-thread
/// plumbing is gone; inbound data is [`TuiEvent`]s from the bridge).
fn ui_loop(
    events: Receiver<TuiEvent>,
    submit_tx: tokio::sync::mpsc::UnboundedSender<String>,
    approve_tx: tokio::sync::mpsc::UnboundedSender<(String, crate::frontend::Approval)>,
    theme_name: String,
) -> io::Result<()> {
    // Build the render context: interactive TTY, sized to the terminal.
    let width = crossterm::terminal::size().map(|(w, _)| w as usize).unwrap_or(80);
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

    let mut state = InteractiveState::new(events, submit_tx, approve_tx, render_context, theme);

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
        let mut redraw = true;
        select! {
            recv(state.events) -> ev => match ev {
                Ok(ev) => {
                    if state.handle_event(ev) {
                        break Ok(());
                    }
                }
                Err(_) => break Ok(()), // bridge dropped → daemon gone.
            },
            recv(timer_tick) -> _ => {
                // Only the streaming spinner needs the periodic redraw.
                redraw = state.loading.pending_request;
            }
            recv(event_rx) -> event => match event {
                Ok(Event::Key(key)) if !key.is_release() => {
                    if state.handle_key_event(key, &mut terminal) {
                        break Ok(());
                    }
                }
                Ok(Event::Mouse(mouse_event)) => state.handle_mouse_event(mouse_event, &terminal),
                Ok(_) => redraw = false,
                Err(_) => break Ok(()),
            },
        }

        if !redraw {
            continue;
        }

        state.handle_hover();
        state.handle_click();
        terminal.draw(|frame| state.render_frame(frame))?;

        // Keep pinned to the bottom while following the stream. The layout cache
        // is fresh after the draw above, so max_scroll is now known.
        if state.viewport.stuck_to_bottom {
            let max = state.max_scroll();
            if state.viewport.scroll_offset != max {
                state.set_scroll_offset(max);
                terminal.draw(|frame| state.render_frame(frame))?;
            }
        }

        state.update_cursor(&mut terminal);
    };

    // Restore the terminal.
    disable_raw_mode()?;
    if state.ui.supports_cursor {
        set_cursor_shape(terminal.backend_mut(), "default");
    }
    execute!(terminal.backend_mut(), LeaveAlternateScreen, DisableMouseCapture)?;
    terminal.show_cursor()?;
    result
}

/// Bullet characters for list items at different nesting levels.
/// Cycles through these as lists nest deeper. (Vendored from ferritin's
/// `renderer::mod`, which lived above the interactive renderer.)
const LIST_BULLETS: &[char] = &['◦', '▪', '•', '‣', '⁃'];

/// Get the bullet character for a given indentation level.
///
/// The indent is the column position; each nesting level adds ~4 columns
/// (2 spaces + bullet + space).
pub(crate) fn bullet_for_indent(indent: u16) -> char {
    let nesting_level = (indent / 4) as usize;
    LIST_BULLETS[nesting_level % LIST_BULLETS.len()]
}
