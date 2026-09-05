//! Compiled OpenAI model capabilities (docs/transport.md).
//!
//! OpenAI's Models API reports identity and availability, but not the context,
//! output, modality, reasoning, or cache facts the boot path consumes. Entries
//! here come from the official model catalog and remain exact: aliases are
//! listed deliberately, and an unknown model errors rather than inheriting a
//! nearby family's limits.

use super::{ModelCapabilities, PromptCachePolicy};
use crate::api::Error;

const GPT_5_6_EFFORTS: &[&str] = &["none", "low", "medium", "high", "xhigh", "max"];

/// Capabilities for an OpenAI model known to this build.
pub(crate) fn model_capabilities(model: &str) -> Result<ModelCapabilities, Error> {
    match model {
        "gpt-5.6" | "gpt-5.6-sol" | "gpt-5.6-terra" | "gpt-5.6-luna" => Ok(ModelCapabilities {
            context_length: 1_050_000,
            max_output_tokens: Some(128_000),
            supports_reasoning: true,
            supports_image_input: true,
            prompt_cache: PromptCachePolicy::Explicit,
            supported_efforts: Some(
                GPT_5_6_EFFORTS
                    .iter()
                    .map(|effort| (*effort).to_string())
                    .collect(),
            ),
            default_effort: Some("medium".to_string()),
        }),
        _ => Err(format!(
            "OpenAI model {model:?} has no capability profile in this efference build; \
             known models are gpt-5.6, gpt-5.6-sol, gpt-5.6-terra, and gpt-5.6-luna"
        )
        .into()),
    }
}
