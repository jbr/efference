//! Compiled OpenAI model capabilities (docs/transport.md).
//!
//! OpenAI's Models API reports identity and availability, but not the context,
//! output, modality, reasoning, or cache facts the boot path consumes. Entries
//! here come from the official model catalog and remain exact: aliases are
//! listed deliberately, and an unknown model errors rather than inheriting a
//! nearby family's limits.

use super::{ModelCapabilities, PromptCachePolicy, Provider};
use crate::api::Error;

const GPT_5_6_EFFORTS: &[&str] = &["none", "low", "medium", "high", "xhigh", "max"];
const GPT_5_3_CODEX_EFFORTS: &[&str] = &["low", "medium", "high", "xhigh"];
/// Astra drops `none`; the guide's migration note sends `none` users to `low`.
const GPT_6_ASTRA_EFFORTS: &[&str] = &["low", "medium", "high", "xhigh", "max"];

/// Every model this build knows, for the error that names them.
pub const KNOWN_MODELS: &[&str] = &[
    "gpt-5.6",
    "gpt-5.6-sol",
    "gpt-5.6-terra",
    "gpt-5.6-luna",
    "gpt-5.3-codex-spark",
    "gpt-6-astra",
];

/// The catalog's cheapest entry, the spotter default on either OpenAI arm
/// (docs/permissions.md).
pub const CHEAPEST_MODEL: &str = "gpt-5.6-luna";

fn efforts(list: &[&str]) -> Option<Vec<String>> {
    Some(list.iter().map(|effort| (*effort).to_string()).collect())
}

/// Capabilities for an OpenAI model known to this build.
pub fn model_capabilities(provider: Provider, model: &str) -> Result<ModelCapabilities, Error> {
    match model {
        "gpt-5.6" | "gpt-5.6-sol" | "gpt-5.6-terra" | "gpt-5.6-luna" => Ok(ModelCapabilities {
            context_length: 1_050_000,
            max_output_tokens: Some(128_000),
            supports_reasoning: true,
            supports_image_input: true,
            accepts_temperature: true,
            prompt_cache: PromptCachePolicy::Explicit,
            supported_efforts: efforts(GPT_5_6_EFFORTS),
            default_effort: Some("medium".to_string()),
            // Nothing in this catalog declares a reasoning requirement, and
            // the Responses encoder serializes no `thinking` field for it to
            // govern (`api/responses.rs`), so the value is inert here.
            reasoning_mandatory: None,
        }),
        // Codex Spark is a ChatGPT Pro research preview on the subscription
        // backend, not an API model. Its launch profile is text-only with a
        // 128k context window; the Codex docs use medium effort in configured
        // agent examples. The backend owns its separate quota bucket.
        "gpt-5.3-codex-spark" if provider == Provider::OpenAICodex => Ok(ModelCapabilities {
            context_length: 128_000,
            max_output_tokens: None,
            supports_reasoning: true,
            supports_image_input: false,
            accepts_temperature: false,
            prompt_cache: PromptCachePolicy::Explicit,
            supported_efforts: efforts(GPT_5_3_CODEX_EFFORTS),
            default_effort: Some("medium".to_string()),
            reasoning_mandatory: None,
        }),
        "gpt-5.3-codex-spark" => Err(
            "OpenAI model \"gpt-5.3-codex-spark\" is available only on the openai-codex provider"
                .into(),
        ),
        // From the model page (1,050,000 context, 128,000 max output, text and
        // image input, efforts low–max) and the Astra guide: no `none` effort,
        // and `temperature` must be omitted. The page names no default effort.
        "gpt-6-astra" => Ok(ModelCapabilities {
            context_length: 1_050_000,
            max_output_tokens: Some(128_000),
            supports_reasoning: true,
            supports_image_input: true,
            accepts_temperature: false,
            prompt_cache: PromptCachePolicy::Explicit,
            supported_efforts: efforts(GPT_6_ASTRA_EFFORTS),
            default_effort: None,
            reasoning_mandatory: None,
        }),
        _ => Err(format!(
            "OpenAI model {model:?} has no capability profile in this efference build; \
             known models are {}",
            KNOWN_MODELS.join(", ")
        )
        .into()),
    }
}
