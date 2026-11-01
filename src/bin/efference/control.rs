//! CLI and stdio-MCP mounts for the library control tools (docs/operating.md).

use std::path::PathBuf;

use efference::clients::control_tools::{Control, Executor, Fallback, MCP_INSTRUCTIONS, cli, mcp};

use super::CliResult;

/// Run `efference control <tool>` and print what it returned. A verb that takes an
/// optional `--agent` and gets none drives the one session the host lists here.
pub(crate) async fn run_cli(tools: cli::Tools, host: PathBuf) -> CliResult {
    let executor = Executor::client(host, Fallback::Discover);
    let state = Control::new(executor, true);
    let text = mcplease::traits::Dispatch::call_to_text(
        tools,
        state,
        mcplease::types::RequestContext::default(),
    )
    .await?;
    // `status --follow` has printed as it went.
    if !text.is_empty() {
        println!("{text}");
    }
    Ok(())
}

/// Run `efference mcp`: the agent verbs over newline-delimited JSON-RPC on
/// stdin/stdout, the transport a harness mounting this server speaks
/// (docs/operating.md). Nothing is resolved at startup: the host need not be
/// running yet, and each call names its agent. Every call is spawned,
/// so a `wait` leaves the others free; the agents the mount watches for `wait`
/// are this process's, kept until it exits. Returns once stdin closes and every
/// call has been answered.
pub(crate) async fn run_stdio(host: PathBuf) -> CliResult {
    let state = Control::new(Executor::client(host, Fallback::None), false);
    let config =
        mcplease::ServerConfig::new(mcplease::server_info!()).with_instructions(MCP_INSTRUCTIONS);
    let stdin = async_compat::Compat::new(tokio::io::stdin());
    let stdout = async_compat::Compat::new(tokio::io::stdout());
    let mut calls = mcplease::calls::<mcp::Tools, _>(stdin, stdout, state, &config);
    while let Some(call) = calls.next().await {
        tokio::spawn(call?);
    }
    Ok(())
}

#[cfg(test)]
mod test;
