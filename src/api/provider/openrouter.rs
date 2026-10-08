//! OpenRouter's model catalog, normalized (docs/model-catalog.md).
//!
//! `GET /api/v1/models` answers the whole text-model catalog in one body — 464
//! models and 762 KB at last read. Each entry is reduced to a
//! [`ListedModel`]: the routable id, the author it groups under, the display
//! name, and the facts the entry declares.
//!
//! An author is the id's prefix (`anthropic` in `anthropic/claude-…`). A leading
//! `~` marks OpenRouter's rolling aliases (`~anthropic/claude-sonnet-latest`);
//! the group folds them under the plain author, and the model ids stay as the
//! catalog spells them.
//!
//! Two kinds of entry are left out. An entry whose `supported_parameters` does
//! not name `tools` cannot run a session's tool loop. A `:batch` variant names
//! `tools` but is a batch route, which does not serve an interactive loop. Both
//! stay reachable as a typed id.

use super::row::{ListedModel, ModelKind, Price, arrange};
use super::*;

#[derive(Deserialize)]
struct Listing {
    data: Vec<Entry>,
}

#[derive(Deserialize)]
struct Entry {
    id: String,
    // No `default`: the name is how this body tells itself from Anthropic's
    // listing shape, which OpenRouter answers a request carrying
    // `anthropic-version` with (`display_name` instead).
    name: String,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    created: Option<i64>,
    #[serde(default)]
    context_length: Option<u64>,
    #[serde(default)]
    pricing: Option<Pricing>,
    #[serde(default)]
    architecture: Option<Architecture>,
    #[serde(default)]
    supported_parameters: Option<Vec<String>>,
    #[serde(default)]
    reasoning: Option<serde_json::Value>,
}

#[derive(Deserialize)]
struct Pricing {
    #[serde(default)]
    prompt: Option<String>,
    #[serde(default)]
    completion: Option<String>,
}

#[derive(Deserialize)]
struct Architecture {
    #[serde(default)]
    input_modalities: Option<Vec<String>>,
    #[serde(default)]
    output_modalities: Option<Vec<String>>,
}

/// The author an id belongs to, alias marker removed. `None` for an id with no
/// `/`, which has no author to group under.
pub(super) fn author_of(id: &str) -> Option<&str> {
    let (author, _) = id.split_once('/')?;
    Some(author.strip_prefix('~').unwrap_or(author))
}

/// A display name without the author prefix OpenRouter prepends
/// (`Z.ai: GLM 5.3 Flash` → `GLM 5.3 Flash`). A name with no `": "` stands.
fn strip_author(name: &str) -> &str {
    name.split_once(": ").map_or(name, |(_, rest)| rest)
}

impl Entry {
    /// A decision model declares `decisions` as an output modality and no
    /// supported parameters at all, so it is told apart before the `tools` test.
    fn kind(&self) -> ModelKind {
        let decisions = self
            .architecture
            .as_ref()
            .and_then(|a| a.output_modalities.as_ref())
            .is_some_and(|modalities| modalities.iter().any(|m| m == "decisions"));
        if decisions {
            ModelKind::Decisions
        } else {
            ModelKind::Text
        }
    }

    fn offered(&self) -> bool {
        if self.id.ends_with(":batch") {
            return false;
        }
        if self.kind() == ModelKind::Decisions {
            return true;
        }
        self.supported_parameters
            .as_ref()
            .is_none_or(|parameters| parameters.iter().any(|p| p == "tools"))
    }

    fn row(self) -> ListedModel {
        let reasoning = match (&self.reasoning, &self.supported_parameters) {
            (Some(value), _) if !value.is_null() => Some(true),
            (_, Some(parameters)) => Some(parameters.iter().any(|p| p == "reasoning")),
            _ => None,
        };
        let kind = self.kind();
        ListedModel {
            group: author_of(&self.id).map(str::to_string),
            created: self.created,
            kind,
            // Some listings report `0` for a window they do not state.
            context: self.context_length.filter(|context| *context > 0),
            price: self.pricing.as_ref().and_then(|pricing| {
                Price::from_per_token(pricing.prompt.as_deref()?, pricing.completion.as_deref()?)
            }),
            reasoning,
            images: self
                .architecture
                .and_then(|a| a.input_modalities)
                .map(|modalities| modalities.iter().any(|m| m == "image")),
            ..ListedModel::new(&self.id)
                .named(strip_author(&self.name))
                .described(self.description.as_deref())
        }
    }
}

/// What one model does, from its `GET /api/v1/models/{author}/{slug}/endpoints`
/// body: `data.architecture.output_modalities` names `decisions` for a decision
/// model and `text` for a generative one.
pub(super) fn kind_of_endpoints(body: &str) -> Result<ModelKind, Error> {
    #[derive(Deserialize)]
    struct Body {
        data: Data,
    }
    #[derive(Deserialize)]
    struct Data {
        architecture: Architecture,
    }
    let body: Body = serde_json::from_str(body)
        .map_err(|e| -> Error { format!("model metadata is not model metadata: {e}").into() })?;
    let decisions = body
        .data
        .architecture
        .output_modalities
        .is_some_and(|modalities| modalities.iter().any(|m| m == "decisions"));
    Ok(if decisions {
        ModelKind::Decisions
    } else {
        ModelKind::Text
    })
}

/// The rows of a `GET /api/v1/models` body, filtered and ordered.
pub(super) fn rows(body: &str) -> Result<Vec<ListedModel>, Error> {
    let listing: Listing = serde_json::from_str(body)
        .map_err(|e| -> Error { format!("model catalog is not a model catalog: {e}").into() })?;
    Ok(arrange(
        listing
            .data
            .into_iter()
            .filter(Entry::offered)
            .map(Entry::row)
            .collect(),
    ))
}

#[cfg(test)]
mod test;
