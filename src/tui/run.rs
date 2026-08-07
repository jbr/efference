//! Entry points — attach to a daemon, or run the element gallery — and theme resolution.

use super::*;

pub async fn run(client: Client, theme: Option<String>) -> Result<()> {
    use crate::frontend::Approval;

    // Validate the theme up front, before the alt-screen takes over, so a bad
    // name surfaces a clean error (with the list) instead of a silent fallback.
    let theme = resolve_theme(theme)?;

    let (tx, rx) = crossbeam_channel::unbounded::<TuiEvent>();
    let (submit_tx, mut submit_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    let (approve_tx, mut approve_rx) = tokio::sync::mpsc::unbounded_channel::<(String, Approval)>();
    let (pace_tx, mut pace_rx) = tokio::sync::mpsc::unbounded_channel::<PaceCmd>();
    let (interrupt_tx, mut interrupt_rx) =
        tokio::sync::mpsc::unbounded_channel::<crate::frontend::Interrupt>();

    // The bridge feeds the UI thread everything — log entries, live deltas, and the
    // state topics (gate/processes/usage/policy/pace/running) as level-triggered
    // frames on the same connection (docs/control-api.md). It ends when the stream
    // closes or the UI thread drops the receiver. No poll: the daemon pushes a
    // whole-topic replacement when a topic changes, and the connect snapshot seeds
    // every topic up front.
    let bridge = tokio::spawn(bridge(client.clone(), tx.clone()));

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

    // Pace sender: each pace key issues a `POST /pace` (docs/tui.md).
    let pace_client = client.clone();
    let pacer = tokio::spawn(async move {
        while let Some((speed_ms, held)) = pace_rx.recv().await {
            let _ = pace_client.pace(speed_ms, held).await;
        }
    });

    // Interrupt sender: the operator's intent over the in-flight tool. `Kill`
    // (the hard rung of graduated Esc, docs/tui.md) fires `POST /interrupt`;
    // `Detach` (the foreground→background handoff, docs/supervisor.md) fires
    // `POST /background`.
    let interrupt_client = client.clone();
    let interrupter = tokio::spawn(async move {
        while let Some(intent) = interrupt_rx.recv().await {
            let _ = match intent {
                crate::frontend::Interrupt::Kill => interrupt_client.interrupt().await,
                crate::frontend::Interrupt::Detach => interrupt_client.background(None).await,
            };
        }
    });

    // The crossterm UI loop is blocking — run it off the async runtime.
    let ui = tokio::task::spawn_blocking(move || {
        let senders = Senders {
            submit: submit_tx,
            approve: approve_tx,
            pace: pace_tx,
            interrupt: interrupt_tx,
        };
        ui_loop(rx, senders, theme, None)
    });

    let result = ui.await.context("the TUI thread panicked")?;
    bridge.abort();
    sender.abort();
    approver.abort();
    pacer.abort();
    interrupter.abort();
    result.context("the TUI event loop failed")
}

/// Launch the TUI on the fixed [`gallery`] transcript — every element type, no
/// daemon.
pub async fn run_gallery(theme: Option<String>) -> Result<()> {
    run_offline(gallery::log(), None, theme).await
}

/// Launch the TUI **read-only** on a recorded session's log (`efference view`,
/// docs/tui.md): the same transcript rendering as a live attach, over entries
/// loaded from a jsonl file rather than a daemon's event stream. `label` names the
/// session in the status bar. Nothing here can write — the log is read once, and
/// the read-only state renders no composer and carries no loop verbs.
pub async fn run_view(entries: Vec<LogEntry>, label: String, theme: Option<String>) -> Result<()> {
    run_offline(entries, Some(label), theme).await
}

/// The shared offline runner behind [`run_gallery`] and [`run_view`]: seed the
/// events channel with a fixed entry list and run the real [`ui_loop`] over it with
/// no daemon behind it. The events sender is held for the session's life so the UI
/// never sees a `Disconnected` (which would exit immediately); the
/// submit/approve/pace/interrupt far-ends are likewise held so a send from inside
/// the state can't error. Real `ui_loop`, real rendering — just an offline log.
async fn run_offline(
    entries: Vec<LogEntry>,
    viewing: Option<String>,
    theme: Option<String>,
) -> Result<()> {
    let theme = resolve_theme(theme)?;

    let (tx, rx) = crossbeam_channel::unbounded::<TuiEvent>();
    let (submit_tx, submit_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    let (approve_tx, approve_rx) =
        tokio::sync::mpsc::unbounded_channel::<(String, crate::frontend::Approval)>();
    let (pace_tx, pace_rx) = tokio::sync::mpsc::unbounded_channel::<PaceCmd>();
    let (interrupt_tx, interrupt_rx) =
        tokio::sync::mpsc::unbounded_channel::<crate::frontend::Interrupt>();

    for entry in entries {
        let _ = tx.send(TuiEvent::Entry(entry));
    }

    let ui = tokio::task::spawn_blocking(move || {
        let senders = Senders {
            submit: submit_tx,
            approve: approve_tx,
            pace: pace_tx,
            interrupt: interrupt_tx,
        };
        ui_loop(rx, senders, theme, viewing)
    });
    let result = ui.await.context("the TUI thread panicked")?;

    // Hold the far-ends until the UI exits, so the channels stay open for its life.
    drop(tx);
    drop(submit_rx);
    drop(approve_rx);
    drop(pace_rx);
    drop(interrupt_rx);

    result.context("the TUI event loop failed")
}

/// Run the interactive TUI against a daemon control socket.
///
/// Subscribes to the daemon's unified `/events` stream (`?include=log,output,state`,
/// docs/control-api.md) over the [`Client`], bridges its frames into the UI thread,
/// and renders the transcript until the user quits or the daemon goes away. The
/// crossterm event loop blocks, so the UI runs on a dedicated blocking thread while
/// the SSE bridge runs as a tokio task (docs/README.md — the TUI is one client of the
/// socket).
/// The default theme — nicer than `THEME_NAMES[0]` ("1337", which only wins by
/// sort order). Overridable with `--theme` / `EFFERENCE_THEME`.
pub(super) const DEFAULT_THEME: &str = "Catppuccin Frappe";

/// Resolve the theme to apply. An explicit name must exist in the built-in set
/// (or be a `.tmTheme` path) — an unknown one is a hard error listing the
/// options. With no override, prefer [`DEFAULT_THEME`], falling back to whatever
/// sorts first if the theme set doesn't include it (e.g. submodules not fetched).
pub(super) fn resolve_theme(theme: Option<String>) -> Result<String> {
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
