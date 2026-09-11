//! The binary — the control-channel daemon entry point.
//!
//! The CLI is three groups (docs/operating.md): the human verbs at the top level,
//! `efference control <tool>` / `efference mcp` for the twelve calls that drive a
//! running session, and `efference dev <verb>` for the instruments that develop the
//! harness itself.
//!
//! `efference serve` opens a JSONL session log, binds the control socket, and runs
//! the agent loop with the HTTP-fed [`RemoteFrontend`] (docs/README.md, docs/control-channel.md):
//! the socket is the singular authoritative input source — the first user turn
//! arrives via `POST /turns`, not argv. `--model` defaults to the Haiku band, which keeps
//! the blast radius of an unconfigured run cheap; an operator names the band they want.
//!
//! [`RemoteFrontend`]: efference::daemon::RemoteFrontend

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

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
    let client_socket = || resolve_client_socket(name.as_deref(), socket_override.clone());
    match cli.command {
        Command::Serve {
            session,
            bind,
            host,
            tls_cert,
            tls_key,
            open,
        } => {
            // `serve` owns the session dir: `--name` or a generated timestamp id → `.efference/<id>/`,
            // with the socket/log/config colocated inside.
            let id = cli.name.unwrap_or_else(default_session_id);
            serve(
                session_dir(&id),
                session,
                serve::BindFlags {
                    bind,
                    host,
                    tls_cert,
                    tls_key,
                    open,
                },
            )
            .await
        }
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
        Command::Control(tools) => control::run_cli(tools, client_socket()?).await,
        Command::Mcp => control::run_stdio(client_socket()?).await,
        #[cfg(feature = "dev")]
        Command::Dev(command) => dev::run(command, name.clone(), client_socket).await,
        Command::Check { path } => check(path).await,
        Command::Tui {
            new: true,
            host_socket,
            session,
            theme,
            images,
            frame_cost,
        } => {
            let host_socket =
                host_socket.unwrap_or_else(|| Path::new(BASE_DIR).join(serve::HOST_SOCKET_NAME));
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
            tui(client_socket()?, theme, images.into(), frame_cost).await
        }
    }
}

type CliResult = Result<(), Box<dyn std::error::Error + Send + Sync>>;

use check::*;
use cli::*;
#[cfg(feature = "dev")]
use paint::*;
use serve::serve;
use verbs::*;

#[cfg(feature = "dev")]
mod cache_report;
mod check;
mod cli;
mod control;
#[cfg(feature = "dev")]
mod dev;

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
