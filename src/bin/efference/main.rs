//! The binary — the control-channel daemon entry point.
//!
//! The CLI is grouped by who each command is for (docs/operating.md): bare
//! `efference` (the TUI, or with `--host` the host) and the human verbs at the top level,
//! `efference control <tool>` / `efference mcp` for the calls that drive a
//! running session, and `efference dev <verb>` for the instruments that develop the
//! harness itself.
//!
//! `efference --host` binds a control socket and creates sessions over it; each runs
//! the agent loop with the HTTP-fed [`RemoteFrontend`] (docs/README.md, docs/control-channel.md):
//! the socket is the singular authoritative input source — the first user turn
//! arrives via `POST /turns`, not argv. A creation's model selects from the
//! host's configured connections; omitted, past picks apply, else creation
//! refuses and the new-session form names one.
//!
//! [`RemoteFrontend`]: efference::daemon::RemoteFrontend

use std::path::{Path, PathBuf};

use clap::{Parser, Subcommand};
#[cfg(feature = "dev")]
use efference::api::Client;
use efference::client::Client as ControlClient;
use efference::daemon;
#[cfg(feature = "dev")]
use efference::spotter::MountedSpotter;
#[cfg(feature = "dev")]
use efference::spotter::Spotter;
use efference::tools::Toolbox;

type CliError = Box<dyn std::error::Error + Send + Sync>;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let cli = Cli::parse().checked().unwrap_or_else(|error| error.exit());
    let Some(command) = cli.command else {
        if cli.host.run {
            return serve::host(
                cli.host.bind_flags(cli.bind),
                cli.host.socket,
                cli.host.island,
            )
            .await;
        }
        // An explicit name is an answer already; the picker is for the
        // invocation that named nothing (docs/operating.md).
        return match cli.name {
            Some(name) => {
                let target = resolve_client_target(Some(&name)).await?;
                tui(target, display()?).await
            }
            None => setup::default_verb(cli.bind.as_deref()).await,
        };
    };
    match command {
        Command::Setup { config } => setup::run(config, cli.bind.as_deref()).await,
        Command::Control { host_socket, tools } => {
            control::run_cli(tools, host_socket.unwrap_or_else(cli::mount_host)).await
        }
        Command::Mcp { host_socket } => {
            control::run_stdio(host_socket.unwrap_or_else(cli::mount_host)).await
        }
        #[cfg(feature = "dev")]
        Command::Dev(command) => dev::run(command).await,
        Command::Check { path } => check(path).await,
        Command::Auth {
            command: AuthCommand::Rotate,
        } => {
            // Rotation removes the root from the store; a daemon already
            // running holds it in memory and would keep serving after the
            // store no longer has it. A false positive — something answering
            // the socket that is not a host — only skips a rotation, so the
            // probe may over-refuse.
            let socket = cli::mount_host();
            if local_daemon_is_listening(&socket) {
                return Err(format!(
                    "refusing to rotate: a local daemon is listening on {}; stop it first. \
                     Rotating under a live daemon leaves it serving a root the store no \
                     longer has.",
                    socket.display()
                )
                .into());
            }
            let secrets = efference::secrets::Secrets::detect()?;
            efference::auth::Root::rotate(&secrets)?;
            eprintln!(
                "Rotated: the root is gone from {}, and the next `efference --host --bind` \
                 mints another. This {}.",
                secrets.describe(),
                efference::auth::ROTATION_CONSEQUENCES
            );
            Ok(())
        }
    }
}

/// Whether something is answering `socket` right now. A successful connect is
/// the daemon's own evidence of listening — the same liveness rule the client
/// verbs resolve a host by (docs/control-channel.md) — so a stale socket file
/// left by a dead process does not refuse a rotation.
fn local_daemon_is_listening(socket: &Path) -> bool {
    std::os::unix::net::UnixStream::connect(socket).is_ok()
}

type CliResult = Result<(), Box<dyn std::error::Error + Send + Sync>>;

use check::*;
use cli::*;
#[cfg(feature = "dev")]
use paint::*;
use verbs::*;

#[cfg(feature = "dev")]
mod cache_report;
mod check;
mod cli;
mod control;
#[cfg(feature = "dev")]
mod dev;
#[cfg(feature = "dev")]
mod dev_compact;
#[cfg(feature = "dev")]
mod dev_models;
#[cfg(feature = "dev")]
mod inference;
mod island;

#[cfg(feature = "dev")]
mod paint;
mod picker;
#[cfg(feature = "dev")]
mod primer_ladder;
mod serve;
mod setup;
#[cfg(feature = "dev")]
mod spot_report;
#[cfg(feature = "dev")]
mod spotter_eval;
#[cfg(feature = "dev")]
mod stop_report;
#[cfg(feature = "dev")]
mod triage_replay;
mod verbs;

#[cfg(test)]
mod test;
