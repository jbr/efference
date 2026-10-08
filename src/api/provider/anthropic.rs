//! Anthropic's model metadata: the listing, normalized (docs/model-catalog.md),
//! and one model's capabilities (docs/transport.md).
//!
//! `GET /v1/models?limit=100` answers `id`, `display_name`, `created_at` (RFC
//! 3339), `max_input_tokens`, and a `capabilities` object. The image fact reads
//! the same `capabilities.image_input.supported` key the boot-time metadata
//! lookup (`GET /v1/models/{id}`, [`capabilities`]) does, and is left absent
//! when the entry omits it.

use super::row::{ListedModel, arrange, unix_from_rfc3339};
use super::*;

#[derive(Deserialize)]
struct Listing {
    data: Vec<Entry>,
}

#[derive(Deserialize)]
struct Entry {
    id: String,
    #[serde(default)]
    display_name: String,
    #[serde(default)]
    created_at: Option<String>,
    #[serde(default)]
    max_input_tokens: Option<u64>,
    #[serde(default)]
    capabilities: Option<Capabilities>,
    #[serde(default)]
    line: Option<String>,
    #[serde(default)]
    lifecycle: Option<String>,
}

#[derive(Deserialize)]
struct Capabilities {
    #[serde(default)]
    image_input: Option<Supported>,
    #[serde(default)]
    thinking: Option<ThinkingCapability>,
    #[serde(default)]
    effort: Option<EffortCapability>,
}

#[derive(Deserialize)]
struct Supported {
    #[serde(default)]
    supported: Option<bool>,
}

impl Supported {
    fn yes(entry: &Option<Self>) -> bool {
        entry.as_ref().and_then(|s| s.supported) == Some(true)
    }
}

#[derive(Deserialize)]
struct ThinkingCapability {
    #[serde(default)]
    types: ThinkingTypes,
}

#[derive(Deserialize, Default)]
struct ThinkingTypes {
    #[serde(default)]
    adaptive: Option<Supported>,
    #[serde(default)]
    disabled: Option<Supported>,
}

/// `capabilities.effort`: a `supported` flag beside one `{supported}` entry per
/// level. The levels are read as whatever keys the object carries, so a level
/// the API adds is offered without a change here.
#[derive(Deserialize)]
struct EffortCapability {
    #[serde(default)]
    supported: bool,
    #[serde(flatten)]
    levels: std::collections::BTreeMap<String, Option<Supported>>,
}

/// The order the boot line and a refusal name the levels in. A level outside it
/// sorts after these, alphabetically.
const EFFORT_ORDER: [&str; 5] = ["low", "medium", "high", "xhigh", "max"];

/// One model's capabilities from `GET /v1/models/{id}`: `max_input_tokens` is
/// the context window, `max_tokens` the output cap, and `capabilities` carries
/// image input, the `thinking` types the model accepts, and its effort levels.
pub(super) fn capabilities(body: &str) -> Result<ModelCapabilities, Error> {
    #[derive(Deserialize)]
    struct Model {
        max_input_tokens: Option<u64>,
        max_tokens: Option<u64>,
        #[serde(default)]
        capabilities: Option<Capabilities>,
    }
    let model: Model = serde_json::from_str(body)?;
    let context_length = model.max_input_tokens.ok_or_else(|| -> Error {
        "Anthropic model metadata carried no max_input_tokens".into()
    })?;
    let capabilities = model.capabilities;
    let thinking = capabilities
        .as_ref()
        .and_then(|c| c.thinking.as_ref())
        .map_or(ThinkingSupport::UNKNOWN, |t| ThinkingSupport {
            adaptive: Supported::yes(&t.types.adaptive),
            // A model that refuses `disabled` answers it with a 400 naming
            // `between_tools` as the way to turn thinking off; the metadata does
            // not list that type, so the refusal is the signal (docs/transport.md).
            off: if Supported::yes(&t.types.disabled) {
                ThinkingOff::Disabled
            } else {
                ThinkingOff::BetweenTools
            },
        });
    // The levels are the request's own spellings (`output_config.effort`), so a
    // configured level is checked against them at boot. A model whose effort
    // capability is unsupported declares the empty list, which refuses every
    // level; an entry with no effort capability declares nothing and checks
    // nothing (docs/transport.md).
    let supported_efforts = capabilities
        .as_ref()
        .and_then(|c| c.effort.as_ref())
        .map(|effort| {
            let mut levels: Vec<String> = effort
                .levels
                .iter()
                .filter(|(_, entry)| effort.supported && Supported::yes(entry))
                .map(|(level, _)| level.clone())
                .collect();
            levels.sort_by_key(|level| {
                EFFORT_ORDER
                    .iter()
                    .position(|known| known == level)
                    .unwrap_or(EFFORT_ORDER.len())
            });
            levels
        });
    Ok(ModelCapabilities {
        context_length: Some(context_length),
        max_output_tokens: model.max_tokens,
        thinking,
        // Absent defaults to **true**, unlike the ceilings above, which error
        // on absence. The ceilings have no safe default (a guessed number is
        // silently wrong); this one does — the Messages API has accepted image
        // blocks on every Claude model since it shipped, so a metadata response
        // that stops carrying the field must not disable a working tool.
        supports_image_input: capabilities
            .as_ref()
            .and_then(|c| c.image_input.as_ref())
            .and_then(|i| i.supported)
            .unwrap_or(true),
        accepts_temperature: true,
        // Anthropic's `cache_control` markers let the harness choose the write
        // frontier (docs/transport.md).
        prompt_cache: PromptCachePolicy::Explicit,
        validates_efforts: supported_efforts.is_some(),
        supported_efforts,
        // The metadata names which levels a model takes, not which one it
        // defaults to.
        default_effort: None,
    })
}

/// The rows of a `GET /v1/models` body, newest first.
pub(super) fn rows(body: &str) -> Result<Vec<ListedModel>, Error> {
    let listing: Listing = serde_json::from_str(body)
        .map_err(|e| -> Error { format!("model listing is not a model listing: {e}").into() })?;
    Ok(arrange(
        listing
            .data
            .into_iter()
            .map(|entry| ListedModel {
                created: entry.created_at.as_deref().and_then(unix_from_rfc3339),
                context: entry.max_input_tokens,
                images: entry
                    .capabilities
                    .and_then(|c| c.image_input)
                    .and_then(|i| i.supported),
                line: entry.line,
                lifecycle: entry.lifecycle,
                ..ListedModel::new(&entry.id).named(&entry.display_name)
            })
            .collect(),
    ))
}

#[cfg(test)]
mod test;
