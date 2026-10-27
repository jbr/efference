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

/// Runs the command, and prints a failure's `Display` to stderr under the program's
/// name. A `main` returning the error would print its `Debug` form instead: quoted,
/// with every newline escaped.
#[tokio::main]
async fn main() -> std::process::ExitCode {
    match run().await {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("efference: {error}");
            std::process::ExitCode::FAILURE
        }
    }
}

async fn run() -> CliResult {
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
        Command::Devices { command } => devices::run(command).await,
        Command::Auth {
            command: AuthCommand::Rotate,
        } => rotate(),
    }
}

/// `efference auth rotate`: store a fresh root, then delete every enrolled device and
/// pending sign-in (docs/web-bind.md).
fn rotate() -> CliResult {
    // Rotation replaces the root in the store; a daemon already running holds
    // the old one in memory and would keep serving it after the store moved on. A
    // false positive — something answering the socket that is not a host — only
    // skips a rotation, so the probe may over-refuse.
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
    let secrets = efference::secrets::Secrets::for_user()?;
    efference::auth::Root::rotate(&secrets)?;
    // The root goes first: no token verifies under the new one, so a table that
    // cannot be cleared leaves rows that sign nothing in, never a token that does.
    let removed = efference::devices::Store::user_scoped()
        .clear()
        .map_err(|e| {
            format!(
                "rotated: {} holds a fresh root, so no device token verifies any \
                 more, but the enrolled devices could not be deleted ({e}), and \
                 `efference devices` lists them until they are revoked",
                secrets.describe()
            )
        })?;
    // A sign-in approved under the old root would otherwise enroll under the next
    // one, if claimed within its lifetime.
    efference::sign_ins::Store::user_scoped()
        .clear()
        .map_err(|e| {
            format!(
                "rotated, and {removed} enrolled device{} deleted, but the pending \
                 sign-ins could not be ({e}); one approved in the last {} s can still \
                 enroll a browser under the next root",
                if removed == 1 { " is" } else { "s are" },
                efference::sign_ins::TTL_SECS
            )
        })?;
    eprintln!(
        "Rotated: {} holds a fresh root, which the next `efference --host --bind` serves, \
         and {removed} enrolled device{} deleted. This {}.",
        secrets.describe(),
        if removed == 1 { " is" } else { "s are" },
        efference::auth::ROTATION_CONSEQUENCES
    );
    Ok(())
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
mod devices;
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
