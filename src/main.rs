//! Phase 0 — single-turn seam check.
//!
//! Builds the request from `project(log)` and folds the API response back into
//! the log as a `Payload::Assistant`, so the substrate and the transport now
//! meet over one real turn. Still no loop, no tools, no cache breakpoint — those
//! are the next slice (loop + bash + approval).
//!
//! Runs against Haiku to keep the blast radius cheap; DESIGN §8 locks
//! `claude-opus-4-8` as the real default.

use std::time::{SystemTime, UNIX_EPOCH};

use efference::api::Client;
use efference::log::{ContentBlock, LogEntry, Payload, Timestamp};
use efference::project::project;

const MODEL: &str = "claude-haiku-4-5";

fn now() -> Timestamp {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock before Unix epoch")
        .as_millis() as u64;
    Timestamp(millis)
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let api_key = std::env::var("ANTHROPIC_API_KEY")
        .map_err(|_| "ANTHROPIC_API_KEY is not set in the environment")?;
    let client = Client::new(api_key, MODEL);

    // Seed the log with a single user turn.
    let mut log: Vec<LogEntry> = vec![LogEntry {
        seq: 0,
        turn: 0,
        at: now(),
        payload: Payload::User(vec![ContentBlock::Text(
            "Reply with exactly: transport spike online".into(),
        )]),
    }];

    // project(log) -> wire -> API -> parsed assistant turn.
    let completion = client.complete(&project(&log)).await?;

    // Fold the assistant turn back into the log: the seam is closed.
    log.push(LogEntry {
        seq: 1,
        turn: 1,
        at: now(),
        payload: completion.assistant,
    });

    println!("model: {}", completion.model);
    println!("stop_reason: {:?}", completion.stop_reason);
    println!("usage: {:?}", completion.usage);
    println!("log now has {} entries", log.len());
    // Re-project the grown log so the round-trip through the substrate is visible.
    println!("--- projected transcript ---");
    println!("{}", serde_json::to_string_pretty(&project(&log))?);
    Ok(())
}
