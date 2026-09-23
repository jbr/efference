//! CLI and stdio-MCP mounts for the library control tools (docs/operating.md).

use std::path::PathBuf;

use efference::control_tools::{Control, Executor, Fallback, INSTRUCTIONS, Tools};

use super::CliResult;

/// Run `efference control <tool>` and print what it returned. A call that names no
/// session resolves one from `--name`/`--socket`/discovery, as the other client
/// verbs do.
pub(crate) async fn run_cli(
    tools: Tools,
    host: PathBuf,
    name: Option<String>,
    socket: Option<PathBuf>,
) -> CliResult {
    let executor = Executor::client(host, Fallback::Resolve { name, socket });
    let state = Control::new(executor, true);
    let text = mcplease::traits::Dispatch::call_to_text(
        tools,
        state,
        mcplease::types::RequestContext::default(),
    )
    .await?;
    println!("{text}");
    Ok(())
}

/// Run `efference mcp`: the shared tools over newline-delimited JSON-RPC on
/// stdin/stdout, the transport a harness mounting this server speaks
/// (docs/operating.md). Nothing is resolved at startup: the host need not be
/// running yet, and each call names its session — unless `--name`/`--socket`
/// was given, which a call that names none then drives. Every call is spawned,
/// so a `turn` waiting on its session leaves the others free. Returns once
/// stdin closes and every call has been answered.
pub(crate) async fn run_stdio(
    host: PathBuf,
    name: Option<String>,
    socket: Option<PathBuf>,
) -> CliResult {
    let fallback = if name.is_some() || socket.is_some() {
        Fallback::Resolve { name, socket }
    } else {
        Fallback::None
    };
    let state = Control::new(Executor::client(host, fallback), false);
    let config =
        mcplease::ServerConfig::new(mcplease::server_info!()).with_instructions(INSTRUCTIONS);
    let stdin = async_compat::Compat::new(tokio::io::stdin());
    let stdout = async_compat::Compat::new(tokio::io::stdout());
    let mut calls = mcplease::calls::<Tools, _>(stdin, stdout, state, &config);
    while let Some(call) = calls.next().await {
        tokio::spawn(call?);
    }
    Ok(())
}
