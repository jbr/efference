//! The human top-level verbs that talk to a running daemon or read a recorded
//! session (docs/operating.md).
//!
//! The thirteen verbs that *drive* a session live in `control/` instead, as one
//! mcplease tool each; what is left here is what a person types: the two TUI
//! entry points and the bootstrap-link minter.

use super::*;

/// `efference tui --new`: create a session on a running host and attach to it
/// (docs/control-api.md). This process's cwd is the session's working root and
/// its environment the session's base; the host prepares the session (its boot
/// report goes to the host's stderr) and answers with the id, which is then the
/// attach prefix `/sessions/<id>/`. Quitting the TUI detaches; the session
/// outlives it on the host.
pub(crate) async fn tui_new(
    host_socket: PathBuf,
    name: Option<String>,
    flags: SessionFlags,
    theme: Option<String>,
    images: efference::tui::ImageMode,
    frame_cost: bool,
) -> CliResult {
    let cwd = std::env::current_dir()?;
    let env = efference::env_table::EnvSnapshot::capture()?.into_entries();
    let triage = flags.triage();
    let SessionFlags {
        model,
        spotter_model,
        config,
        temperature,
        effort,
        upstream,
        autonomous,
        reauth,
        keep_scratch,
        ..
    } = flags;
    let request = efference::daemon::CreateSession {
        name,
        cwd: cwd.clone(),
        config,
        model,
        spotter_model,
        triage,
        temperature,
        effort,
        upstream,
        autonomous,
        reauth,
        keep_scratch,
        env,
    };
    eprintln!("creating a session on {}…", host_socket.display());
    let id = ControlClient::new(&host_socket)
        .create_session(&request)
        .await?;
    // The session dir is where the host minted it: `<cwd>/.efference/<id>`, the
    // same layout boot uses (docs/control-channel.md), so the attachment store
    // resolves as at a socket attach.
    let attachments = efference::attachments::dir_for_session(&cwd.join(session_dir(&id)));
    efference::tui::run(
        ControlClient::at(host_socket, &format!("/sessions/{id}")),
        theme,
        images,
        frame_cost,
        Some(attachments),
    )
    .await?;
    Ok(())
}

/// Launch the interactive TUI against the daemon control socket. All rendering
/// lives in `efference::tui`; here we just hand it a control client and the
/// optional theme override.
pub(crate) async fn tui(
    target: ClientTarget,
    theme: Option<String>,
    images: efference::tui::ImageMode,
    frame_cost: bool,
) -> CliResult {
    // The attachment store lives in the session dir (docs/inbox.md), so a paste past
    // the inline threshold has somewhere local to land. A socket with no parent
    // leaves the TUI storeless and pasting inline.
    let attachments = target
        .session_dir()
        .map(|dir| efference::attachments::dir_for_session(&dir));
    efference::tui::run(target.client(), theme, images, frame_cost, attachments).await?;
    Ok(())
}
