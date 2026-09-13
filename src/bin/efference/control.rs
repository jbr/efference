//! CLI and stdio-MCP mounts for the library control tools (docs/operating.md).

use std::path::PathBuf;

use efference::control_tools::{Control, Executor, Fallback, INSTRUCTIONS, Tools};
use tokio::runtime::Handle;

use super::CliResult;

/// Run `efference control <tool>` and print what it returned. A call that names no
/// session resolves one from `--name`/`--socket`/discovery, as the other client
/// verbs do. Dispatch is synchronous, so it runs on a blocking thread rather than
/// on a runtime worker.
pub(crate) async fn run_cli(
    tools: Tools,
    host: PathBuf,
    name: Option<String>,
    socket: Option<PathBuf>,
) -> CliResult {
    let runtime = Handle::current();
    let text = tokio::task::spawn_blocking(move || {
        let executor = Executor::client(runtime, host, Fallback::Resolve { name, socket });
        let mut state = Control::new(executor, true);
        mcplease::traits::Dispatch::call_to_text(
            tools,
            &mut state,
            &mcplease::types::RequestContext::default(),
        )
    })
    .await??;
    println!("{text}");
    Ok(())
}

/// Run `efference mcp`: the shared tools over newline-delimited JSON-RPC on
/// stdin/stdout, the transport a harness mounting this server speaks
/// (docs/operating.md). Nothing is resolved at startup: the host need not be
/// running yet, and each call names its session — unless `--name`/`--socket`
/// was given, which a call that names none then drives. Blocks until stdin
/// closes.
pub(crate) async fn run_stdio(
    host: PathBuf,
    name: Option<String>,
    socket: Option<PathBuf>,
) -> CliResult {
    let runtime = Handle::current();
    let fallback = if name.is_some() || socket.is_some() {
        Fallback::Resolve { name, socket }
    } else {
        Fallback::None
    };
    tokio::task::spawn_blocking(move || {
        let executor = Executor::client(runtime, host, fallback);
        let mut state = Control::new(executor, false);
        let config =
            mcplease::ServerConfig::new(mcplease::server_info!()).with_instructions(INSTRUCTIONS);
        mcplease::serve::<Tools, _>(&mut state, &config)
    })
    .await??;
    Ok(())
}
