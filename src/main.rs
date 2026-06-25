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
use efference::client::Client as ControlClient;
use efference::daemon;
use efference::distill::ForkPeek;
use efference::frontend::Approval;
use efference::persist::Log;
use efference::tools::Toolbox;
use futures_lite::StreamExt;
use tokio::io::{AsyncBufReadExt, BufReader};

/// The default control-socket path, under the gitignored `.efference/`.
const DEFAULT_SOCKET: &str = ".efference/control.sock";

#[derive(Parser)]
#[command(name = "efference", about = "An API-backed agentic coding harness.")]
struct Cli {
    /// Path to the control socket — shared by the daemon and every client verb.
    #[arg(long, global = true, default_value = DEFAULT_SOCKET)]
    socket: PathBuf,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Run the daemon: bind the control socket and drive the agent loop. Waits for
    /// the first user turn over the socket (`POST /turn`).
    Serve {
        /// Model to use for the agent. Defaults to claude-haiku-4-5.
        #[arg(long, default_value = "claude-haiku-4-5")]
        model: String,
    },
    /// Send a user turn to a running daemon (`POST /turn`). Seeds turn 1 on an
    /// idle daemon. Returns immediately — watch `state`/`log` for the response.
    Turn {
        /// The turn text. Multiple words are joined with spaces.
        #[arg(required = true, num_args = 1..)]
        text: Vec<String>,
    },
    /// Resolve the pending tool-approval gate (`POST /approvals/{id}`). With no
    /// flags, approves. The call id defaults to the one pending approval.
    Approve {
        /// The `tool_use` call id. Defaults to the daemon's current pending gate.
        call_id: Option<String>,
        /// Reject and unwind the whole turn instead of approving.
        #[arg(long, conflicts_with = "redirect")]
        abort: bool,
        /// Reject this call and steer the model with this message (kept as the next
        /// user turn) instead of approving.
        #[arg(long)]
        redirect: Option<String>,
    },
    /// Interrupt a running tool (`POST /interrupt`) — kills its process group.
    Interrupt,
    /// Interactive client: tail the event stream and drive the daemon from one
    /// terminal. Type a message to send a turn; at an approval gate, Enter
    /// approves, `.abort` aborts, any other text redirects. Ctrl-D quits.
    Attach,
    /// Print the daemon's ephemeral control state (`GET /state`).
    State,
    /// Print the durable append-only log (`GET /log?since=<seq>`).
    Log {
        /// Only entries with `seq >= since`.
        #[arg(long)]
        since: Option<u64>,
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
    let cli = Cli::parse();
    match cli.command {
        Command::Serve { model } => serve(cli.socket, &model).await,
        Command::Turn { text } => turn(cli.socket, text.join(" ")).await,
        Command::Approve {
            call_id,
            abort,
            redirect,
        } => approve(cli.socket, call_id, abort, redirect).await,
        Command::Interrupt => interrupt(cli.socket).await,
        Command::State => state(cli.socket).await,
        Command::Log { since } => log(cli.socket, since).await,
        Command::Attach => attach(cli.socket).await,
    }
}

type CliResult = Result<(), Box<dyn std::error::Error + Send + Sync>>;

async fn turn(socket: PathBuf, text: String) -> CliResult {
    ControlClient::new(socket).turn(&text).await?;
    println!("accepted");
    Ok(())
}

async fn approve(
    socket: PathBuf,
    call_id: Option<String>,
    abort: bool,
    redirect: Option<String>,
) -> CliResult {
    let client = ControlClient::new(socket);
    // Default to the one pending gate (the loop is sequential, DESIGN §8) so the
    // common case is a bare `efference approve`.
    let call_id = match call_id {
        Some(id) => id,
        None => pending_call_id(&client).await?,
    };
    let decision = match (abort, redirect) {
        (true, _) => Approval::Abort,
        (false, Some(message)) => Approval::Redirect(message),
        (false, None) => Approval::Approve,
    };
    client.approve(&call_id, &decision).await?;
    println!("resolved {call_id}");
    Ok(())
}

/// Read the daemon's one pending approval out of `GET /state`.
async fn pending_call_id(
    client: &ControlClient,
) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
    let state: serde_json::Value = serde_json::from_str(&client.state().await?)?;
    state
        .get("pending_approval")
        .and_then(|p| p.get("call_id"))
        .and_then(|id| id.as_str())
        .map(str::to_string)
        .ok_or_else(|| "no pending approval; pass a call id explicitly".into())
}

async fn interrupt(socket: PathBuf) -> CliResult {
    ControlClient::new(socket).interrupt().await?;
    println!("interrupted");
    Ok(())
}

async fn state(socket: PathBuf) -> CliResult {
    let body = ControlClient::new(socket).state().await?;
    println!("{}", pretty(&body));
    Ok(())
}

async fn log(socket: PathBuf, since: Option<u64>) -> CliResult {
    let body = ControlClient::new(socket).log(since).await?;
    println!("{}", pretty(&body));
    Ok(())
}

/// Re-emit a JSON body pretty-printed; falls back to the raw string if it isn't
/// JSON (so an error body still prints legibly).
fn pretty(body: &str) -> String {
    serde_json::from_str::<serde_json::Value>(body)
        .ok()
        .map(|v| serde_json::to_string_pretty(&v).unwrap_or_else(|_| body.to_string()))
        .unwrap_or_else(|| body.to_string())
}

/// The `attach` REPL — a spartan interactive client (DESIGN §10: authoritative-
/// but-spartan, the full TUI is a later client). A background task renders the
/// live event tail; the foreground reads stdin and, per line, either resolves the
/// pending approval gate or sends a new turn. Both halves are just clients of the
/// socket — no privileged path (DESIGN §8).
async fn attach(socket: PathBuf) -> CliResult {
    let client = ControlClient::new(socket);

    // Background: tail `/events` from the start of the session and render each
    // entry as it lands. Dropped when `attach` returns (process exit).
    // Background: render the durable event tail and the ephemeral delta stream in
    // ONE task. Merging them (rather than two racing tasks) lets the live-streamed
    // prose suppress its own committed echo: when a turn streamed text, the
    // committed entry's text blocks are omitted — only its tool-call/approval lines,
    // which are never streamed, still render. See `render_streams`.
    let tail = client.clone();
    let handle = tokio::spawn(async move {
        if let Err(e) = render_streams(tail).await {
            eprintln!("[render loop ended: {e}]");
        }
    });

    println!(
        "attached. type a message to send a turn. at an approval gate: Enter approves, \
         '.abort' aborts, any other text redirects. Ctrl-D to quit."
    );

    let mut lines = BufReader::new(tokio::io::stdin()).lines();
    while let Some(line) = lines.next_line().await? {
        let text = line.trim().to_string();
        // Check the gate per line: the loop is sequential, so there is at most one
        // pending call (DESIGN §8), and stdin is dual-use — approval when a gate is
        // open (mirroring the old interactive frontend's Enter-approves policy),
        // otherwise a new turn.
        match pending_call_id(&client).await.ok() {
            Some(call_id) => {
                let decision = match text.as_str() {
                    "" => Approval::Approve,
                    ".abort" => Approval::Abort,
                    other => Approval::Redirect(other.to_string()),
                };
                client.approve(&call_id, &decision).await?;
                println!("[gate {call_id} → {decision:?}]");
            }
            None if text.is_empty() => {}
            None => {
                client.turn(&text).await?;
                println!("[turn sent]");
            }
        }
    }

    handle.abort();
    Ok(())
}

/// What the merged renderer has already shown live, so a committed entry can skip
/// re-printing it. Both flags are set as fragments stream and cleared when the
/// corresponding committed entry arrives.
#[derive(Default)]
struct RenderState {
    /// A turn's assistant prose was streamed → omit its committed text blocks.
    streamed_assistant: bool,
    /// A tool's output was streamed → close that live region before the result.
    streamed_tool: bool,
}

/// Render the durable event tail (`GET /events`) and the ephemeral delta stream
/// (`GET /stream`) from a single task (DESIGN §10). Deltas physically precede the
/// commit they belong to (the daemon publishes them during inference, then appends
/// the entry), so the live fragments print first and the committed entry simply
/// finalizes — without the duplicated block the two-task version produced.
async fn render_streams(client: ControlClient) -> CliResult {
    let mut events = client.events(Some(0)).await?;
    let mut deltas = client.stream().await?;
    let mut state = RenderState::default();
    let mut deltas_done = false;
    loop {
        tokio::select! {
            event = events.next() => match event {
                Some(event) => {
                    let event = event?;
                    render_event(event.event_type(), event.data(), &mut state);
                }
                None => break, // the durable tail ended (daemon down) → stop.
            },
            // Guarded so a finished delta stream (None forever) can't busy-loop the
            // select; the durable tail keeps rendering on its own after.
            delta = deltas.next(), if !deltas_done => match delta {
                Some(delta) => render_delta(delta?, &mut state),
                None => deltas_done = true,
            },
        }
    }
    Ok(())
}

/// Render one ephemeral delta: append its fragment to the current line and flush
/// (no newline, so prose / output accrete as they stream). The first fragment of a
/// region prints a label and trips the matching [`RenderState`] flag so the
/// committed entry knows to omit its echo.
fn render_delta(event: trillium_client::sse::Event, state: &mut RenderState) {
    let value: serde_json::Value = serde_json::from_str(event.data()).unwrap_or_default();
    match event.event_type() {
        Some("assistant_delta") => {
            if let Some(text) = value["text"].as_str() {
                if !state.streamed_assistant {
                    print!("\n◂ assistant: ");
                    state.streamed_assistant = true;
                }
                print_flush(text);
            }
        }
        Some("tool_output") => {
            if let Some(chunk) = value["chunk"].as_str() {
                if !state.streamed_tool {
                    print!("\n· output: ");
                    state.streamed_tool = true;
                }
                print_flush(chunk);
            }
        }
        _ => {}
    }
}

fn print_flush(s: &str) {
    print!("{s}");
    let _ = std::io::Write::flush(&mut std::io::stdout());
}

/// Render one SSE event (a serialized [`LogEntry`]) as a concise line. Navigates
/// the JSON loosely (via `Value`) so the CLI stays decoupled from the log's
/// internal shape. `state` carries what was already shown live, so a streamed turn
/// finalizes without re-printing its prose (the dedup the merged renderer exists
/// for).
fn render_event(kind: Option<&str>, data: &str, state: &mut RenderState) {
    let Ok(entry) = serde_json::from_str::<serde_json::Value>(data) else {
        println!("· {data}");
        return;
    };
    let seq = entry["seq"].as_u64().unwrap_or(0);
    let payload = &entry["payload"];
    match kind {
        Some("user") => {
            let text = payload["User"]
                .as_array()
                .and_then(|blocks| blocks.first())
                .and_then(|b| b["Text"].as_str())
                .unwrap_or("");
            println!("\n#{seq} ▸ user: {text}");
        }
        Some("assistant") => {
            let assistant = &payload["Assistant"];
            // If the prose was streamed live, just close the live line and skip the
            // text blocks — only the never-streamed tool-call lines still render.
            if state.streamed_assistant {
                println!();
                state.streamed_assistant = false;
            } else {
                for block in assistant["text"].as_array().into_iter().flatten() {
                    if let Some(s) = block["Text"].as_str()
                        && !s.is_empty()
                    {
                        println!("\n#{seq} ◂ assistant: {s}");
                    }
                }
            }
            for call in assistant["calls"].as_array().into_iter().flatten() {
                let name = call["name"].as_str().unwrap_or("?");
                let input = call["input"].to_string();
                println!(
                    "\n#{seq} ◂ wants {name}: {input}  (Enter approves · text redirects · .abort)"
                );
            }
        }
        Some("tool_result") => {
            // Close the live output region (if any) before the committed result —
            // which is the fork *distillate*, not a dup of the raw streamed output.
            if state.streamed_tool {
                println!();
                state.streamed_tool = false;
            }
            let result = &payload["ToolResult"];
            let tag = if result["raw"]["is_error"].as_bool().unwrap_or(false) {
                "error"
            } else {
                "result"
            };
            // The wire is `{"Distilled": "…"}`, `"Raw"`, or `"Elided"`; show the
            // distillate when present, else the raw content.
            let shown = result["wire"]["Distilled"]
                .as_str()
                .or_else(|| result["raw"]["content"].as_str())
                .unwrap_or("");
            println!("\n#{seq} · tool {tag}:\n{shown}");
        }
        _ => println!("\n#{seq} · {data}"),
    }
}

async fn serve(
    socket: PathBuf,
    model: &str,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
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

    let client = Client::new(api_key, model, system);

    // The model-driven fork (DESIGN §4): every tool result is peeked and the
    // canonical wire keeps only what the model retains. `Toolbox` is the §5 tool
    // seam; mount bash + the in-process MCP servers (fs-mcp + semantic-edit-mcp),
    // their session cwd seeded to where we're running.
    let cwd = std::env::current_dir()?;
    let toolbox = Toolbox::bash_and_mcps(cwd)?;
    daemon::serve(client, toolbox, ForkPeek, log, socket).await
}
