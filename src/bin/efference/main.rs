//! The binary — the control-channel daemon entry point.
//!
//! The CLI is three groups (docs/operating.md): the human verbs at the top level,
//! `efference control <tool>` / `efference mcp` for the twelve calls that drive a
//! running session, and `efference dev <verb>` for the instruments that develop the
//! harness itself.
//!
//! `efference host` binds a control socket and creates sessions over it; each runs
//! the agent loop with the HTTP-fed [`RemoteFrontend`] (docs/README.md, docs/control-channel.md):
//! the socket is the singular authoritative input source — the first user turn
//! arrives via `POST /turns`, not argv. `--model` selects from the host's
//! configured connections; omitted, the host's default applies.
//!
//! [`RemoteFrontend`]: efference::daemon::RemoteFrontend

use std::path::{Path, PathBuf};

use clap::{Parser, Subcommand};
use efference::api::Client;
use efference::client::Client as ControlClient;
use efference::daemon;
use efference::distill::TriageModel;
use efference::spotter::ForkSpot;
#[cfg(feature = "dev")]
use efference::spotter::Spotter;
use efference::tools::Toolbox;

type CliError = Box<dyn std::error::Error + Send + Sync>;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let cli = Cli::parse();
    // The `control`/`mcp` groups and the client verbs talk to a daemon over a socket resolved
    // from `--name`/`--socket`/discovery; the daemon and the offline `dev` verbs don't. Resolve
    // lazily, per arm, so an offline verb never triggers discovery.
    let name = cli.name.clone();
    let socket_override = cli.socket.clone();
    let client_target = || resolve_client_target(name.as_deref(), socket_override.clone());
    match cli.command {
        Command::Host {
            bind,
            host,
            tls_cert,
            tls_key,
            open,
        } => {
            serve::host(serve::BindFlags {
                bind,
                host,
                tls_cert,
                tls_key,
                open,
            })
            .await
        }
        Command::Control(tools) => control::run_cli(tools, client_target().await?).await,
        Command::Mcp => control::run_stdio(client_target().await?).await,
        #[cfg(feature = "dev")]
        Command::Dev(command) => dev::run(command, name.clone(), client_target).await,
        Command::Check { path } => check(path).await,
        Command::Tui {
            new: true,
            host_socket,
            session,
            theme,
            images,
            frame_cost,
        } => {
            let host_socket = host_socket.unwrap_or_else(cli::host_socket);
            tui_new(
                host_socket,
                cli.name.clone(),
                session,
                theme,
                images.into(),
                frame_cost,
            )
            .await
        }
        Command::Tui {
            new: false,
            session,
            theme,
            images,
            frame_cost,
            ..
        } => {
            if session.any_given() {
                return Err("session flags (--model, --config, …) need --new: \
                     without it `tui` attaches to a running session"
                    .into());
            }
            tui(client_target().await?, theme, images.into(), frame_cost).await
        }
    }
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
mod inference;

#[cfg(feature = "dev")]
mod paint;
#[cfg(feature = "dev")]
mod peek_replay;
mod serve;
#[cfg(feature = "dev")]
mod spot_report;
#[cfg(feature = "dev")]
mod spotter_eval;
mod verbs;

#[cfg(test)]
mod test;
