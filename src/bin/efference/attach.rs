//! `efference attach` — decode the daemon's two SSE streams to the terminal (docs/control-channel.md).

use super::*;

/// The `attach` REPL — a spartan interactive client (docs/README.md: authoritative-
/// but-spartan, the full TUI is a later client). A background task renders the
/// live event tail; the foreground reads stdin and, per line, either resolves the
/// pending approval gate or sends a new turn. Both halves are just clients of the
/// socket — no privileged path (docs/control-channel.md).
pub(crate) async fn attach(socket: PathBuf) -> CliResult {
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
        // pending call (docs/README.md), and stdin is dual-use — approval when a gate is
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
pub(crate) struct RenderState {
    /// A turn's assistant prose was streamed → omit its committed text blocks.
    pub(crate) streamed_assistant: bool,
    /// A tool's output was streamed → close that live region before the result.
    pub(crate) streamed_tool: bool,
}

/// Render the durable event tail (`GET /events`) and the ephemeral delta stream
/// (`GET /stream`) from a single task (docs/README.md). Deltas physically precede the
/// commit they belong to (the daemon publishes them during inference, then appends
/// the entry), so the live fragments print first and the committed entry simply
/// finalizes — without the duplicated block the two-task version produced.
pub(crate) async fn render_streams(client: ControlClient) -> CliResult {
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
pub(crate) fn render_delta(event: trillium_client::sse::Event, state: &mut RenderState) {
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

pub(crate) fn print_flush(s: &str) {
    print!("{s}");
    let _ = std::io::Write::flush(&mut std::io::stdout());
}

/// Render one SSE event (a serialized [`LogEntry`]) as a concise line. Navigates
/// the JSON loosely (via `Value`) so the CLI stays decoupled from the log's
/// internal shape. `state` carries what was already shown live, so a streamed turn
/// finalizes without re-printing its prose (the dedup the merged renderer exists
/// for).
pub(crate) fn render_event(kind: Option<&str>, data: &str, state: &mut RenderState) {
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
            // The wire is `{"Distilled": {keep, note}}`, `"Raw"`, or `"Elided"`. A
            // distilled result is re-rendered from raw + the stored verdict
            // (docs/redaction-fork.md — the bytes aren't stored); Raw/Elided show the raw content.
            let raw_content = result["raw"]["content"].as_str().unwrap_or("");
            let shown = if let Some(d) = result["wire"].get("Distilled") {
                let keep: efference::log::Keep =
                    serde_json::from_value(d["keep"].clone()).unwrap_or(efference::log::Keep::All);
                efference::project::render_distilled(raw_content, &keep, d["note"].as_str())
            } else {
                raw_content.to_string()
            };
            println!("\n#{seq} · tool {tag}:\n{shown}");
        }
        _ => println!("\n#{seq} · {data}"),
    }
}
