//! Building the separate-model peek from a model name (docs/redaction-fork.md) —
//! one constructor for the daemon's boot and the replay tool, so the replayed shape
//! cannot drift from the live one.

use super::*;
use crate::api::{Endpoint, ModelCapabilities};
use std::time::Duration;

/// `--triage-model` and its two knobs, as the CLI collects them and as a host's
/// creation body spells them.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct TriageModel {
    pub model: String,
    #[serde(default)]
    /// `--triage-thinking`: let the model reason (off by default; the reasoning is
    /// discarded with the fork's tail).
    pub thinking: bool,
    #[serde(default)]
    /// `--triage-auto-choice`: `tool_choice: auto` instead of forcing the peek tool.
    pub auto_choice: bool,
    #[serde(default)]
    /// `--triage-effort`: the reasoning level, in the model's own vocabulary
    /// (validated against its metadata like `--effort`). Implies thinking on: a
    /// model that can be turned down rather than off is the case this serves.
    pub effort: Option<String>,
}

impl ForkPeek {
    /// The separate-model peek for `triage` (docs/redaction-fork.md): the model's
    /// capabilities looked up like the agent's, a client on [`Endpoint::for_triage`]
    /// with the given `system`, thinking off unless asked, a peek-sized output cap,
    /// and the asked-for tool forced unless asked otherwise. Returns the capabilities
    /// beside the peek so the caller can print the window. `system` is the caller's:
    /// [`crate::env::triage_system_prompt`], masked by whatever policy applies.
    pub async fn for_triage_model(
        endpoint: &Endpoint,
        triage: &TriageModel,
        system: String,
        stream_ceiling: Duration,
    ) -> Result<(ForkPeek, ModelCapabilities), String> {
        let caps = endpoint
            .fetch_capabilities(&triage.model, &[])
            .await
            .map_err(|e| {
                format!(
                    "model metadata lookup for {:?} on {} failed: {e}",
                    triage.model,
                    endpoint.provider.name()
                )
            })?;
        if let Some(effort) = &triage.effort {
            caps.check_effort(effort, &triage.model)?;
        }
        let mut client = Client::new(endpoint.for_triage(), &triage.model, system, None)
            .with_stream_ceiling(stream_ceiling)
            .with_prompt_cache(caps.prompt_cache)
            .with_effort(triage.effort.clone());
        client = client.with_thinking(caps.thinking);
        if !triage.thinking && triage.effort.is_none() {
            client = client.without_thinking();
        }
        // A verdict is one small call: cap the reservation at the peek's size, not
        // the model's, so it never crowds the window (`PEEK_MAX_OUTPUT`).
        let cap = caps
            .max_output_tokens
            .map_or(PEEK_MAX_OUTPUT, |cap| cap.min(PEEK_MAX_OUTPUT));
        client = client
            .with_max_output(cap)
            .with_context_window(caps.context_length);
        let peek = ForkPeek::own(client);
        let peek = if triage.auto_choice {
            peek.with_auto_choice()
        } else {
            peek
        };
        Ok((peek, caps))
    }
}
