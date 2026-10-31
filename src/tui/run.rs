//! Entry points — attach to a daemon, or run the element gallery — and theme resolution.

use super::attention::Attender;
use super::dispatchers::Dispatchers;
use super::*;
use crate::user_config::TuiConfig;

/// Attach to a session and stay there until the operator leaves it.
///
/// A switch leaves by returning [`Exit::Switch`], and this is the loop that
/// honours it: the previous attach's tasks are already gone when it returns —
/// aborting the bridge drops the SSE stream, which closes the connection — so a
/// switch is a fresh handshake against the next session and never two cursors at
/// once (docs/control-api.md). `client` is re-based onto the new prefix over the
/// same transport, so the socket is dialled once per attach and nothing has to be
/// re-resolved.
pub async fn run(client: Client, display: TuiConfig, mut attached: Attached) -> Result<()> {
    let mut client = client;
    loop {
        match attach(client.clone(), display.clone(), attached.clone()).await? {
            Exit::Quit => return Ok(()),
            // The terminal is restored by now, so the line lands on the shell.
            Exit::Closed => {
                eprintln!("{}", closed::ask(&client).await.line());
                return Ok(());
            }
            Exit::Switch { id } => {
                client = client.with_prefix(&format!("/sessions/{id}"));
                attached.draft = None;
            }
            Exit::SwitchWithDraft { id, draft } => {
                client = client.with_prefix(&format!("/sessions/{id}"));
                attached.draft = Some(draft);
            }
        }
    }
}

/// What this client is to the sessions it attaches, as opposed to how it draws them.
#[derive(Debug, Clone, Default)]
pub struct Attached {
    /// This client launched the host, and quitting it ends the host with every
    /// session on it (docs/operating.md): the island owner. `detach` would be
    /// false there, so the strip says `quit`.
    pub owns_host: bool,
    /// Text for the composer as the attach opens: the turns a failed start handed
    /// back, carried to the session its retry made (`starting.rs`).
    pub draft: Option<String>,
}

/// One attach: open the stream, run the UI, and tear every task down on the way
/// out. Returns why the loop ended.
async fn attach(client: Client, display: TuiConfig, attached: Attached) -> Result<Exit> {
    // Validate the theme up front, before the alt-screen takes over, so a bad
    // name surfaces a clean error (with the list) instead of a silent fallback.
    let display = resolved(display)?;
    // Open the event stream up front for the same reason: a socket nobody is
    // listening on, or a host prefix that names no session, is a plain error
    // here rather than an empty screen whose turns go nowhere.
    let events = client.events(Some(0), Some("log,output,state")).await?;

    let (tx, rx) = crossbeam_channel::unbounded::<TuiEvent>();
    let (senders, receivers) = Senders::channels();
    let mut dispatchers = Dispatchers::spawn(&client, events, receivers, &tx);
    let attender = Attender::new(
        attention::process_client_id(),
        client.session_id().map(str::to_string),
        std::time::Instant::now(),
    );

    // The crossterm UI loop is blocking — run it off the async runtime.
    let ui = tokio::task::spawn_blocking(move || {
        ui_loop(rx, senders, &display, None, &attached, Some(attender))
    });

    let result = ui.await.context("the TUI thread panicked")?;
    // The loop's senders are gone, so the task ends once it has sent what the
    // loop left queued (the inactive report on quitting); bounded so a stalled
    // host cannot hold the process open.
    let _ = tokio::time::timeout(Duration::from_secs(1), &mut dispatchers.attention).await;
    dispatchers.abort();
    result.context("the TUI event loop failed")
}

/// Launch the TUI on the fixed [`gallery`] transcript — every element type, no
/// daemon. With `stream`, a scripted emitter also plays live tool-call events into
/// the session on a timer (`gallery/stream.rs`), which is how the animation-rate
/// render behaviours get driven without an upstream.
pub async fn run_gallery(
    display: TuiConfig,
    stream: Option<gallery::stream::Script>,
) -> Result<()> {
    run_offline(gallery::log(), None, display, stream, false).await
}

/// Launch the TUI **read-only** on a recorded session's log (`efference dev view`,
/// docs/tui.md): the same transcript rendering as a live attach, over entries
/// loaded from a jsonl file rather than a daemon's event stream. `label` names the
/// session in the status bar. Nothing here can write — the log is read once, and
/// the read-only state renders no composer and carries no loop verbs.
pub async fn run_view(entries: Vec<LogEntry>, label: String, display: TuiConfig) -> Result<()> {
    // The read-only view does not time its paints, whatever `[tui] frame-cost` says.
    let display = TuiConfig {
        frame_cost: false,
        ..display
    };
    run_offline(entries, Some(label), display, None, false).await
}

/// The shared offline runner behind [`run_gallery`] and [`run_view`]: seed the
/// events channel with a fixed entry list and run the real [`ui_loop()`] over it with
/// no daemon behind it. The events sender is held for the session's life so the UI
/// never sees a `Disconnected` (which would exit immediately); the
/// submit/approve/pace/interrupt far-ends are likewise held so a send from inside
/// the state can't error. Real `ui_loop`, real rendering — just an offline log.
async fn run_offline(
    entries: Vec<LogEntry>,
    viewing: Option<String>,
    display: TuiConfig,
    stream: Option<gallery::stream::Script>,
    form_prototype: bool,
) -> Result<()> {
    let display = resolved(display)?;

    let (tx, rx) = crossbeam_channel::unbounded::<TuiEvent>();
    // The far ends are held for the run's life rather than consumed: an offline
    // log's images ride the entries themselves (docs/control-api.md) and there
    // is no daemon for the rest to reach, so nothing should answer them — but a
    // dropped receiver would make a send from inside the state an error.
    let (senders, receivers) = Senders::channels();

    for entry in entries {
        let _ = tx.send(TuiEvent::Entry(entry));
    }

    #[cfg(feature = "dev")]
    if form_prototype {
        let _ = tx.send(TuiEvent::SessionFormPrototype);
    }
    #[cfg(not(feature = "dev"))]
    let _ = form_prototype;

    // The emitter shares the events channel with the seeded log, so live fragments
    // land in the same UI as committed entries — the point being to watch the
    // handoff between them.
    let emitter = stream.map(|script| {
        let tx = tx.clone();
        tokio::spawn(script.play(tx))
    });

    let ui = tokio::task::spawn_blocking(move || {
        ui_loop(rx, senders, &display, viewing, &Attached::default(), None)
    });
    let result = ui.await.context("the TUI thread panicked")?;

    if let Some(emitter) = emitter {
        emitter.abort();
    }

    // Held until the UI exits, so the channels stay open for its life.
    drop(tx);
    drop(receivers);

    result.map(|_| ()).context("the TUI event loop failed")
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
/// sort order). Overridable with `[tui] theme` in the user configuration.
pub(super) const DEFAULT_THEME: &str = "Catppuccin Frappe";

/// `display` with its theme resolved ([`resolve_theme`]), which is what [`ui_loop()`] reads.
fn resolved(display: TuiConfig) -> Result<TuiConfig> {
    Ok(TuiConfig {
        theme: Some(resolve_theme(display.theme)?),
        ..display
    })
}

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

/// Offline Sessions container for the shared form experiment (docs/tui.md).
#[cfg(feature = "dev")]
pub async fn run_session_form_prototype() -> Result<()> {
    let display = TuiConfig {
        images: crate::user_config::ImageMode::Off,
        ..TuiConfig::default()
    };
    run_offline(gallery::log(), None, display, None, true).await
}
