//! The binary — the control-channel daemon entry point.
//!
//! `efference serve` opens a JSONL session log, binds the control socket, and runs
//! the agent loop with the HTTP-fed [`RemoteFrontend`] (DESIGN §10 control-channel):
//! the socket is the singular authoritative input source — the first user turn
//! arrives via `POST /turn`, not argv. Runs against Haiku to keep the blast radius
//! cheap; DESIGN §8 locks `claude-opus-4-8` as the real default.
//!
//! [`RemoteFrontend`]: efference::daemon::RemoteFrontend

use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use clap::{Parser, Subcommand};
use efference::api::Client;
use efference::daemon;
use efference::distill::ForkPeek;
use efference::persist::Log;
use efference::tools::Toolbox;

const MODEL: &str = "claude-haiku-4-5";

/// The default control-socket path, under the gitignored `.efference/`.
const DEFAULT_SOCKET: &str = ".efference/control.sock";

#[derive(Parser)]
#[command(name = "efference", about = "An API-backed agentic coding harness.")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Run the daemon: bind the control socket and drive the agent loop. Waits for
    /// the first user turn over the socket (`POST /turn`).
    Serve {
        /// Path to the control socket.
        #[arg(long, default_value = DEFAULT_SOCKET)]
        socket: PathBuf,
    },
}

/// The system preamble. Explains the curation model so the agent stops defending
/// against context bloat the old way (DESIGN §4-realized) — the run that
/// motivated this watched the model reflexively `grep`/`head` its own tool output.
const SYSTEM: &str = "\
You are a coding agent working in a software project. The current working \
directory is the root of that project (a Git repository); you can investigate it \
directly by running shell commands — list files, read sources, search. When a \
question is about this codebase, explore it yourself rather than asking for the \
source: you are already inside it.

You work through a harness that automatically focuses tool \
output for you: after each command, only the parts that matter are kept on your \
transcript. You'll see line-numbered output, sometimes with `… [N lines elided] \
…` markers where less-relevant spans were dropped. Treat what you receive as the \
output and work with it directly — it has already been focused for relevance.

Because the harness focuses output for you, do NOT pre-filter it yourself. Run \
commands plainly — avoid piping to `head`, `tail`, `grep`, `sed`, `awk`, `cut`, \
or similar to trim what comes back. Pre-trimming risks discarding the part you \
needed before you've had a chance to read it; letting the full output come back \
and keeping the relevant parts afterward is strictly better.

For working with files, prefer the dedicated tools over the shell: `read` (read \
whole files), `search` (find a pattern across files), and `list` (directory \
contents). They return focused, structured results the harness can curate \
cleanly — reach for them before `cat`/`grep`/`ls`.

Work one tool call at a time, using what each result taught you to choose the next.";

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    match Cli::parse().command {
        Command::Serve { socket } => serve(socket).await,
    }
}

async fn serve(socket: PathBuf) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let api_key = std::env::var("ANTHROPIC_API_KEY")
        .map_err(|_| "ANTHROPIC_API_KEY is not set in the environment")?;

    // One JSONL file per session under .efference/ (gitignored). The socket lives
    // alongside it; clear any stale socket from a prior run before binding.
    std::fs::create_dir_all(".efference")?;
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock before Unix epoch")
        .as_millis();
    let log_path = format!(".efference/session-{millis}.jsonl");
    let log = Log::create(&log_path)?;
    eprintln!("logging to {log_path}");
    let _ = std::fs::remove_file(&socket); // ignore if absent.

    // Append the session environment block once, here at startup, and freeze it
    // into the system prompt. It must stay byte-stable for the whole session or it
    // would invalidate the cached prefix every turn (DESIGN §3) — so it carries
    // only session-stable facts (cwd, platform, project type), never volatile state
    // like git status. See `env::context`.
    let system = format!("{SYSTEM}\n\n{}", efference::env::context());

    let client = Client::new(api_key, MODEL, system);

    // The model-driven fork (DESIGN §4): every tool result is peeked and the
    // canonical wire keeps only what the model retains. `Toolbox` is the §5 tool
    // seam; mount bash + the in-process MCP servers (fs-mcp + semantic-edit-mcp),
    // their session cwd seeded to where we're running.
    let cwd = std::env::current_dir()?;
    let toolbox = Toolbox::bash_and_mcps(cwd)?;
    daemon::serve(client, toolbox, ForkPeek, log, socket).await
}
