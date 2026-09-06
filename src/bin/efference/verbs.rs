//! The human top-level verbs that talk to a running daemon or read a recorded
//! session (docs/operating.md).
//!
//! The twelve verbs that *drive* a session live in `control/` instead, as one
//! mcplease tool each; what is left here is what a person types: the two TUI
//! entry points and the bootstrap-link minter.

use super::*;

/// Read a recorded session in the TUI, inert (docs/tui.md): resolve the session to
/// a `log.jsonl`, load it with the same `persist::load` a live attach's entries come
/// through, and hand the entries to the read-only TUI. No socket, no network, no
/// writes — and no config, so it works on a session whose config no longer parses or
/// that ran in another directory.
pub(crate) async fn view(
    session: Option<String>,
    name: Option<String>,
    theme: Option<String>,
    images: efference::tui::ImageMode,
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
    efference::tui::run_view(entries, label, theme, images).await?;
    Ok(())
}

/// Launch the interactive TUI against the daemon control socket. All rendering
/// lives in `efference::tui`; here we just hand it a control client and the
/// optional theme override.
pub(crate) async fn tui(
    socket: PathBuf,
    theme: Option<String>,
    images: efference::tui::ImageMode,
    frame_cost: bool,
) -> CliResult {
    // The attachment store lives beside the socket in the session dir (docs/inbox.md), so a
    // paste past the inline threshold has somewhere local to land. A socket with no parent
    // leaves the TUI storeless and pasting inline.
    let attachments = socket.parent().map(efference::attachments::dir_for_session);
    efference::tui::run(
        ControlClient::new(socket),
        theme,
        images,
        frame_cost,
        attachments,
    )
    .await?;
    Ok(())
}

/// Mint and open a fresh bootstrap link against a running daemon's TCP bind
/// (docs/web-bind.md). `--qr` renders the authenticator enrollment QR at the
/// terminal instead — the one place the TOTP secret is ever displayed; `--bearer`
/// prints the program token. Both come over the Unix socket only.
pub(crate) async fn open(socket: PathBuf, print: bool, qr: bool, bearer: bool) -> CliResult {
    let client = ControlClient::new(socket);
    if bearer {
        println!("{}", client.auth_bearer().await?);
        return Ok(());
    }
    if qr {
        let uri = client.auth_otpauth().await?;
        let code = qrcode::QrCode::new(uri.as_bytes())
            .map_err(|e| format!("could not encode the enrollment QR: {e}"))?;
        println!(
            "{}",
            code.render::<qrcode::render::unicode::Dense1x2>().build()
        );
        println!("{uri}");
        return Ok(());
    }
    let url = client.auth_link().await?;
    println!("{url}");
    if !print && webbrowser::open(&url).is_err() {
        eprintln!("[could not open a browser — use the printed link]");
    }
    Ok(())
}
