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
        let mut last_usage = String::new();
        let mut last_policy = String::new();
        let mut last_pace: Option<PaceView> = None;
        let mut last_running: Option<bool> = None;
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
            // The live process list rides the same poll (docs/supervisor.md). Emit only when it
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
            // Usage statistics from the same poll. Emit only when they change.
            if let Some(usage) = parse_usage(&body) {
                let ufp = serde_json::to_string(&usage).unwrap_or_default();
                if ufp != last_usage {
                    last_usage = ufp;
                    if gate_tx.send(TuiEvent::Usage(usage)).is_err() {
                        break;
                    }
                }
            }
            // The effective policy + proposals from the same poll. Emit on change
            // (fingerprinted via Debug — PolicyView isn't Serialize).
            if let Some(policy) = parse_policy(&body) {
                let pfp = format!("{policy:?}");
                if pfp != last_policy {
                    last_policy = pfp;
                    if gate_tx.send(TuiEvent::Policy(policy)).is_err() {
                        break;
                    }
                }
            }
            // The operator's pace from the same poll (docs/tui.md).
            // Emit on change so a CLI `efference pace` (or another client) is reflected too.
            if let Some(pace) = parse_pace(&body)
                && last_pace != Some(pace)
            {
                last_pace = Some(pace);
                if gate_tx.send(TuiEvent::Pace(pace)).is_err() {
                    break;
                }
            }
            // Whether the loop is running (docs/tui.md) — graduated Esc reads it.
            if let Some(running) = parse_running(&body)
                && last_running != Some(running)
            {
                last_running = Some(running);
                if gate_tx.send(TuiEvent::Running(running)).is_err() {
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
        ui_loop(rx, submit_tx, approve_tx, pace_tx, interrupt_tx, theme)
    });

    let result = ui.await.context("the TUI thread panicked")?;
    bridge.abort();
    poller.abort();
    sender.abort();
    approver.abort();
    pacer.abort();
    interrupter.abort();
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
    let (pace_tx, pace_rx) = tokio::sync::mpsc::unbounded_channel::<PaceCmd>();
    let (interrupt_tx, interrupt_rx) =
        tokio::sync::mpsc::unbounded_channel::<crate::frontend::Interrupt>();

    for entry in gallery::log() {
        let _ = tx.send(TuiEvent::Entry(entry));
    }

    let ui = tokio::task::spawn_blocking(move || {
        ui_loop(rx, submit_tx, approve_tx, pace_tx, interrupt_tx, theme)
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
/// Subscribes to the daemon's `/events` (durable log) and `/stream` (ephemeral
/// deltas) over the [`Client`], bridges them into the UI thread, and renders the
/// transcript until the user quits or the daemon goes away. The crossterm event
/// loop blocks, so the UI runs on a dedicated blocking thread while the SSE
/// bridge runs as a tokio task (docs/README.md — the TUI is one client of the socket).
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
