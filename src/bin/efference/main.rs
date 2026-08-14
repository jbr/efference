//! The binary — the control-channel daemon entry point.
//!
//! `efference serve` opens a JSONL session log, binds the control socket, and runs
//! the agent loop with the HTTP-fed [`RemoteFrontend`] (docs/README.md, docs/control-channel.md):
//! the socket is the singular authoritative input source — the first user turn
//! arrives via `POST /turns`, not argv. Runs against Haiku to keep the blast radius
//! cheap; docs/transport.md locks `claude-opus-4-8` as the real default.
//!
//! [`RemoteFrontend`]: efference::daemon::RemoteFrontend

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use clap::{Parser, Subcommand};
use efference::api::Client;
use efference::client::Client as ControlClient;
use efference::daemon;
use efference::distill::ForkPeek;
use efference::frontend::Approval;
use efference::persist::Log;
use efference::spotter::{ForkSpot, Spotter};
use efference::tools::Toolbox;
use futures_lite::StreamExt;
use tokio::io::{AsyncBufReadExt, BufReader};

type CliError = Box<dyn std::error::Error + Send + Sync>;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let cli = Cli::parse();
    // Client verbs talk to a daemon over a socket resolved from `--name`/discovery; the daemon and
    // the offline verbs (spotter/gallery/wire) don't. Resolve lazily, per arm, so an offline verb
    // never triggers discovery.
    let name = cli.name.clone();
    let socket_override = cli.socket.clone();
    let client_socket = || resolve_client_socket(name.as_deref(), socket_override.clone());
    match cli.command {
        Command::Serve {
            model,
            spotter_model,
            config,
            autonomous,
            reauth,
            keep_scratch,
        } => {
            // `serve` owns the session dir: `--name` or a generated timestamp id → `.efference/<id>/`,
            // with the socket/log/config colocated inside.
            let id = cli.name.unwrap_or_else(default_session_id);
            serve(
                session_dir(&id),
                &model,
                &spotter_model,
                config,
                autonomous,
                reauth,
                keep_scratch,
            )
            .await
        }
        Command::Turn { text, wait } => turn(client_socket()?, text.join(" "), wait).await,
        Command::Approve {
            call_id,
            abort,
            redirect,
            wait,
        } => approve(client_socket()?, call_id, abort, redirect, wait).await,
        Command::Settle => settle(client_socket()?).await,
        Command::Interrupt => interrupt(client_socket()?).await,
        Command::Background { note } => background(client_socket()?, note).await,
        Command::Pace {
            speed_ms,
            hold,
            resume,
        } => pace(client_socket()?, speed_ms, hold, resume).await,
        Command::ReviewLevel { level } => review_level(client_socket()?, level).await,
        Command::Notify {
            source,
            body,
            slot,
            replaces,
        } => notify(client_socket()?, source, body.join(" "), slot, replaces).await,
        Command::Dismiss { id } => dismiss(client_socket()?, id).await,
        Command::State => state(client_socket()?).await,
        Command::Log { since } => log(client_socket()?, since).await,
        Command::Spotter {
            scope,
            watch_for,
            paths,
            model,
            probe,
            tool,
            input,
            config,
            show_move,
            no_brief,
            command,
        } => {
            spotter_check(
                scope,
                watch_for,
                paths,
                &model,
                probe,
                tool,
                input,
                config,
                show_move,
                no_brief,
                command.join(" "),
            )
            .await
        }
        Command::SpotterLog {
            path,
            stances,
            summary,
            list,
            no_fallback,
        } => spotter_log(path, stances, summary, list, no_fallback),
        Command::Attach => attach(client_socket()?).await,
        Command::Tui { theme } => tui(client_socket()?, theme).await,
        Command::View { session, theme } => view(session, name.clone(), theme).await,
        Command::Gallery { theme } => gallery(theme).await,
        Command::Wire {
            path,
            messages,
            format,
            model,
            config,
        } => wire(path, messages, format, &model, config).await,
        Command::Peek {
            path,
            seq,
            repeat,
            limit,
            dry_run,
            model,
        } => peek(path, seq, repeat, limit, dry_run, &model).await,
    }
}

type CliResult = Result<(), Box<dyn std::error::Error + Send + Sync>>;

use attach::attach;
use cli::*;
use peek_replay::peek;
use serve::{serve, spotter_check};
use spot_report::*;
use verbs::*;

mod attach;
mod cli;
mod mcp_mount;
mod peek_replay;
mod serve;
mod spot_report;
mod verbs;

#[cfg(test)]
mod test;
