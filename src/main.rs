//! Phase 0 binary — the agent loop entry point.
//!
//! Parses the first user turn from argv (a backgrounded run can't be fed over
//! stdin, DESIGN §10), opens a JSONL session log, wires the live transport to
//! the interactive frontend, and hands off to [`agent::run`]. Runs against Haiku
//! to keep the blast radius cheap; DESIGN §8 locks `claude-opus-4-8` as the real
//! default.

use std::time::{SystemTime, UNIX_EPOCH};

use efference::agent;
use efference::api::Client;
use efference::frontend::Interactive;
use efference::persist::Log;

const MODEL: &str = "claude-haiku-4-5";

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let api_key = std::env::var("ANTHROPIC_API_KEY")
        .map_err(|_| "ANTHROPIC_API_KEY is not set in the environment")?;

    let prompt = std::env::args().skip(1).collect::<Vec<_>>().join(" ");
    if prompt.trim().is_empty() {
        return Err("usage: efference <prompt>   (the first user turn)".into());
    }

    // One JSONL file per session under .efference/ (gitignored).
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock before Unix epoch")
        .as_millis();
    std::fs::create_dir_all(".efference")?;
    let log_path = format!(".efference/session-{millis}.jsonl");
    let log = Log::create(&log_path)?;
    eprintln!("logging to {log_path}");

    let client = Client::new(api_key, MODEL);
    let mut frontend = Interactive::new()?;

    agent::run(&client, &mut frontend, prompt, log).await
}
