//! The binary — the control-channel daemon entry point.
//!
//! The CLI is grouped by who each command is for (docs/operating.md): the human verbs at the top level,
//! `efference control <tool>` / `efference mcp` for the calls that drive a
//! running session, and `efference dev <verb>` for the instruments that develop the
//! harness itself.
//!
//! `efference host` binds a control socket and creates sessions over it; each runs
//! the agent loop with the HTTP-fed [`RemoteFrontend`] (docs/README.md, docs/control-channel.md):
//! the socket is the singular authoritative input source — the first user turn
//! arrives via `POST /turns`, not argv. `--model` selects from the host's
//! configured connections; omitted, past picks apply, else creation refuses
//! and the new-session form names one.
//!
//! [`RemoteFrontend`]: efference::daemon::RemoteFrontend

use std::path::{Path, PathBuf};

use clap::{Parser, Subcommand};
#[cfg(feature = "dev")]
use efference::api::Client;
use efference::client::Client as ControlClient;
use efference::daemon;
use efference::inference::TriageChoice;
#[cfg(feature = "dev")]
use efference::spotter::MountedSpotter;
#[cfg(feature = "dev")]
use efference::spotter::Spotter;
use efference::tools::Toolbox;

type CliError = Box<dyn std::error::Error + Send + Sync>;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let cli = Cli::parse();
    let command = match cli.command {
        Some(command) => command,
        None => return setup::default_verb(cli.bind.as_deref()).await,
    };
    match command {
        Command::Setup { config } => setup::run(config, cli.bind.as_deref()).await,
        Command::Host {
            socket,
            public_url,
            tls_cert,
            tls_key,
            open,
            island,
        } => {
            serve::host(
                serve::BindFlags {
                    bind: cli.bind.clone(),
                    public_url,
                    tls_cert,
                    tls_key,
                    open,
                },
                socket,
                island,
            )
            .await
        }
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
            let secrets = efference::secrets::Secrets::detect()?;
            efference::auth::Root::rotate(&secrets)?;
            eprintln!(
                "Rotated: the root is gone from {}, and the next `efference host --bind` \
                 mints another. This {}.",
                secrets.describe(),
                efference::auth::ROTATION_CONSEQUENCES
            );
            Ok(())
        }
        Command::Tui {
            name,
            new: true,
            host_socket,
            session,
            theme,
            images,
            frame_cost,
        } => {
            let Some(host_socket) = host_socket.or_else(cli::live_host) else {
                let creation = island::Creation::Direct {
                    name,
                    flags: session,
                };
                return island::run(
                    cli.bind.as_deref(),
                    creation,
                    theme,
                    images.into(),
                    frame_cost,
                )
                .await;
            };
            tui_new(host_socket, name, session, theme, images.into(), frame_cost).await
        }
        Command::Tui {
            name,
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
            // An explicit name is an answer already; the picker is for the
            // invocation that named nothing (docs/operating.md).
            match name {
                None => picker::run(theme, images.into(), frame_cost, cli.bind.as_deref()).await,
                Some(name) => {
                    let target = resolve_client_target(Some(&name)).await?;
                    tui(target, theme, images.into(), frame_cost).await
                }
            }
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
mod stop_report;
#[cfg(feature = "dev")]
mod triage_replay;
mod verbs;

#[cfg(test)]
mod test;
