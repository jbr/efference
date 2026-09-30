//! Anthropic's model listing, normalized (docs/model-catalog.md).
//!
//! `GET /v1/models?limit=100` answers `id`, `display_name`, `created_at` (RFC
//! 3339), `max_input_tokens`, and a `capabilities` object. The image fact reads
//! the same `capabilities.image_input.supported` key the boot-time metadata
//! lookup does, and is left absent when the entry omits it.

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
}

#[derive(Deserialize)]
struct Capabilities {
    #[serde(default)]
    image_input: Option<Supported>,
}

#[derive(Deserialize)]
struct Supported {
    #[serde(default)]
    supported: Option<bool>,
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
                ..ListedModel::new(&entry.id).named(&entry.display_name)
            })
            .collect(),
    ))
}

#[cfg(test)]
mod test;
