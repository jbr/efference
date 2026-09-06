//! The human top-level verbs that talk to a running daemon or read a recorded
//! session (docs/operating.md).
//!
//! The twelve verbs that *drive* a session live in `control/` instead, as one
//! mcplease tool each; what is left here is what a person types: the two TUI
//! entry points and the bootstrap-link minter.

use super::*;

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
