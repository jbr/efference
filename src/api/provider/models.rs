//! Reading a connection's model listing as normalized rows (docs/model-catalog.md).
//!
//! Each provider's normalization sits beside its other listing code
//! (`anthropic.rs`, `codex.rs`, `openrouter.rs`, `listing.rs`); this file picks
//! the source for an endpoint. Storage, age, and refresh are the catalog's
//! (`src/catalog.rs`).

use super::row::{ListedModel, ModelKind};
use super::*;

impl Endpoint {
    /// The models this endpoint's provider lists, normalized and ordered. One
    /// read of the provider's listing route; a provider with no listing answers
    /// an empty set.
    pub async fn catalog_rows(&self) -> Result<Vec<ListedModel>, Error> {
        match self.provider {
            Provider::OpenRouter => {
                let url = self
                    .models_url()
                    .ok_or("the openrouter provider has no model catalog route")?;
                // The default listing is text models only; decision models are
                // a separate output modality, asked for by name alongside it.
                let url = format!("{url}?output_modalities=text,decisions");
                openrouter::rows(&self.authed_get(&url).await?)
            }
            // A described endpoint's listing is the same route the credential
            // check and the capability lookup use, so one shape reads all three
            // (`provider/listing.rs`).
            // A System One endpoint publishes no listing: its URL is its one route,
            // so the typed id is the whole menu.
            Provider::Custom(WireDialect::SystemOne) => Ok(Vec::new()),
            Provider::Custom(_) => {
                let url = self.provider.capabilities_url(self.base.as_deref(), "")?;
                listing::rows(&self.authed_get(&url).await?)
            }
            // The subscription backend's catalog, read through the same cache
            // the capability path uses, minus the entries it hides from a menu
            // (`provider/codex.rs`).
            Provider::OpenAICodex => Ok(codex::catalog(self).await?.rows()),
            // The platform listing names every model the key can reach —
            // embeddings, speech, image, and dated snapshots among them —
            // unfiltered: a long menu beats an empty one, and the typed id stays.
            Provider::OpenAI => {
                let body = self.authed_get("https://api.openai.com/v1/models").await?;
                listing::rows(&body)
            }
            Provider::Anthropic => {
                let body = self
                    .authed_get("https://api.anthropic.com/v1/models?limit=100")
                    .await?;
                anthropic::rows(&body)
            }
            Provider::TypeSafe => {
                let url = self
                    .models_url()
                    .ok_or("the typesafe provider has no model listing route")?;
                typesafe::rows(&self.authed_get(&url).await?)
            }
        }
    }

    /// What a spotter model is, from what its provider says about it
    /// (docs/permissions.md) — the one place the choice between the prose rung and
    /// the decision rung is made, so no id convention stands in for it. Also the
    /// existence check for a decision model, which has no capabilities to look up:
    ///
    /// - A provider that serves no turns lists only decision models; the id is
    ///   checked against its listing where it has one.
    /// - OpenRouter's per-model metadata route answers 2xx for an id it serves and
    ///   declares the output modality, so one GET is both the check and the kind.
    /// - Any other provider serves text models only.
    pub async fn spotter_kind(&self, model: &str) -> Result<ModelKind, Error> {
        if !self.provider.serves_inference() {
            self.check_decision_model(model).await?;
            return Ok(ModelKind::Decisions);
        }
        match self.provider {
            Provider::OpenRouter => {
                let url = self.provider.capabilities_url(None, model)?;
                openrouter::kind_of_endpoints(&self.authed_get(&url).await?)
            }
            _ => Ok(ModelKind::Text),
        }
    }

    /// `(id, display name)` pairs for a flat prompt, in the listing's order. An
    /// OpenRouter connection answers none here: its catalog is a few hundred
    /// rows, which a line prompt does not serve.
    pub async fn list_models(&self) -> Result<Vec<(String, String)>, Error> {
        if self.provider == Provider::OpenRouter {
            return Ok(Vec::new());
        }
        Ok(self
            .catalog_rows()
            .await?
            .into_iter()
            .map(|row| {
                let name = row.name.unwrap_or_else(|| row.model.clone());
                (row.model, name)
            })
            .collect())
    }
}

impl Provider {
    /// The group a model id files under in a listing: OpenRouter's author. Other
    /// providers do not group.
    pub fn group_of(self, model: &str) -> Option<String> {
        match self {
            Self::OpenRouter => openrouter::author_of(model).map(str::to_string),
            _ => None,
        }
    }
}
