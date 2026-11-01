//! TypeSafe's model listing, normalized (docs/model-catalog.md).

use super::row::{ListedModel, ModelKind, arrange, unix_from_rfc3339};
use super::*;

#[derive(Deserialize)]
struct Listing {
    models: Vec<Entry>,
}

#[derive(Deserialize)]
struct Entry {
    name: String,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    release_date: Option<String>,
}

pub(super) fn rows(body: &str) -> Result<Vec<ListedModel>, Error> {
    let listing: Listing = serde_json::from_str(body)
        .map_err(|e| -> Error { format!("model listing is not a model listing: {e}").into() })?;
    Ok(arrange(
        listing
            .models
            .into_iter()
            .map(|entry| ListedModel {
                created: entry.release_date.as_deref().and_then(unix_from_rfc3339),
                kind: ModelKind::Decisions,
                ..ListedModel::new(entry.name).described(entry.description.as_deref())
            })
            .collect(),
    ))
}

#[cfg(test)]
mod test;
