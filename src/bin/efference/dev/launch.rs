//! The `dev` verbs that open something: the element gallery, a sign-in link, and a
//! recorded log in the TUI.

use super::*;

/// Launch the TUI on the fixed element gallery — no daemon, no socket. With
/// `stream`, a scripted emitter plays live tool-call events over it on a timer.
pub(crate) async fn gallery(
    display: efference::inference::TuiConfig,
    stream: bool,
    chunk: usize,
    interval: u64,
) -> CliResult {
    let script = stream.then(|| efference::tui::StreamScript {
        chunk,
        interval: std::time::Duration::from_millis(interval),
    });
    efference::tui::run_gallery(display.theme, display.images, script, display.frame_cost).await?;
    Ok(())
}

/// Mint and open a fresh sign-in link against a running daemon's TCP bind
/// (docs/web-bind.md), over the Unix socket only. A script's token is
/// `efference devices add`.
pub(super) async fn open(print: bool) -> CliResult {
    // The credential routes live at the socket root: the first live host's.
    let host = live_host().ok_or("no host is running; start one with `efference --host`")?;
    let client = ControlClient::new(host);
    let url = client.auth_link().await?;
    println!("{url}");
    if !print && webbrowser::open(&url).is_err() {
        eprintln!("[could not open a browser — use the printed link]");
    }
    Ok(())
}

/// Read a recorded session in the TUI, inert (docs/tui.md): resolve the session to
/// a `log.jsonl`, load it with the same `persist::load` a live session's entries come
/// through, and hand the entries to the read-only TUI. No socket, no network, no
/// writes — and no config, so it works on a session whose config no longer parses or
/// that ran in another directory.
pub(super) async fn view(
    session: Option<String>,
    name: Option<String>,
    display: efference::inference::TuiConfig,
) -> CliResult {
    let Some(target) = session.or(name) else {
        return Err(format!(
            "which session? pass a session name, a session dir, or a log.jsonl path.{}",
            session_listing()
        )
        .into());
    };
    let (path, label) = resolve_session_log(&target)?;
    let entries = efference::persist::load(&path)?;
    if entries.is_empty() {
        return Err(format!("{} holds no entries", path.display()).into());
    }
    efference::tui::run_view(entries, label, display.theme, display.images).await?;
    Ok(())
}
