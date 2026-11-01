//! The human entry points that talk to a running host (docs/operating.md).
//!
//! The verbs that *drive* a session live in `control/` instead, as one
//! mcplease tool each; what is left here is what a person types: attaching the
//! TUI, and the display settings every TUI it draws reads.

use super::*;
use efference::config::user_config::TuiConfig;

/// The `[tui]` table of the user configuration (docs/tui.md): what every
/// surface this process draws a TUI on reads, read once at launch. Without a
/// home directory there is no file, and the defaults stand.
pub(crate) fn display() -> Result<TuiConfig, CliError> {
    match std::env::var_os("HOME") {
        Some(home) => Ok(TuiConfig::load(Path::new(&home))?),
        None => Ok(TuiConfig::default()),
    }
}

/// Attach the TUI to a session this process just created in its own directory.
pub(crate) async fn attach_created(
    host_socket: &Path,
    id: &str,
    display: TuiConfig,
    owns_host: bool,
) -> CliResult {
    efference::tui::run(
        ControlClient::at(host_socket.to_path_buf(), &format!("/sessions/{id}")),
        display,
        efference::tui::Attached {
            owns_host,
            ..Default::default()
        },
    )
    .await?;
    Ok(())
}

/// Launch the interactive TUI against the daemon control socket. All rendering
/// lives in `efference::tui`; here we just hand it a control client and the
/// display settings.
pub(crate) async fn tui(target: ClientTarget, display: TuiConfig) -> CliResult {
    efference::tui::run(
        target.client(),
        display,
        efference::tui::Attached::default(),
    )
    .await?;
    Ok(())
}
