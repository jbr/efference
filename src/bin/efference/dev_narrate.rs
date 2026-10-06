//! `efference dev narrate` — run the summary head's narrator step over a recorded
//! session and print the head (docs/summary-head.md).
//!
//! The narrator is the session's own model: the connection and model the log
//! recorded at boot, resolved through the current config, under the session's
//! recorded system prompt. Nothing is appended to the log — a session's log has one
//! writer, the host — so this reads a session, live or not, without touching it.

use super::*;

use efference::api::{Client, UNKNOWN_OUTPUT_CAP};
use efference::log::Payload;

pub(crate) async fn narrate(target: &str, upto: Option<u64>, cut: Option<u64>) -> CliResult {
    let (path, label) = resolve_session_log(target)?;
    let mut entries = efference::persist::load(&path)?;
    if let Some(upto) = upto {
        entries.retain(|e| e.seq <= upto);
    }
    let identity = efference::log::recorded_inference(&entries)
        .ok_or("log has no recorded boot identity; no session model to narrate with")?;
    let model = identity.model.clone();
    let selection = format!("{}/{}", identity.connection, identity.model);
    let system = entries
        .iter()
        .rev()
        .find_map(|e| match &e.payload {
            Payload::Preamble { system, .. } => Some(system.clone()),
            _ => None,
        })
        .ok_or("log predates the boot-preamble snapshot; no recorded system prompt")?;

    let inference = crate::inference::resolve(Some(&selection), None, None)?;
    let (_, mut endpoint) = inference.main();
    if let Some(dir) = path.parent() {
        endpoint.session_id = Some(efference::api::sticky_session_id(dir));
    }
    // The output cap follows the model's report, as the agent client's does: a
    // reasoning model's thinking bills against it, and the client's small default
    // would end the account early.
    let caps = endpoint.fetch_capabilities(&model, &[]).await?;
    let client = Client::new(endpoint, &model, system, None)
        .with_model_reasoning(caps.supports_reasoning)
        .with_prompt_cache(caps.prompt_cache)
        .with_max_output(caps.max_output_tokens.unwrap_or(UNKNOWN_OUTPUT_CAP))
        .with_context_window(caps.context_length);
    let toolbox = efference::tools::Toolbox::bash_and_mcps(std::env::current_dir()?, None)?;
    let tools = efference::agent::tool_definitions(&toolbox);

    eprintln!(
        "narrating {label} on {selection}{}",
        cut.map(|c| format!(", account through seq {c}"))
            .unwrap_or_default()
    );
    let started = std::time::Instant::now();
    let narration = efference::narrate::narrate(&client, &tools, &entries, cut).await?;
    let usage = &narration.usage.usage;
    eprintln!(
        "{:.0}s · input {} (cache read {}, write {}) · output {}",
        started.elapsed().as_secs_f64(),
        usage.input_tokens,
        usage.cache_read_input_tokens,
        usage.cache_creation_input_tokens,
        usage.output_tokens
    );
    println!("{}", narration.head);
    Ok(())
}
