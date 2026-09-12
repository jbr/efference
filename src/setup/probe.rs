//! How setup looks at the world: the environment it runs in, the Codex cache, and
//! the providers themselves (docs/operating.md). The flow only ever sees the
//! trait, so a test answers from a table and never opens a socket.

use std::path::Path;

use crate::api::{Endpoint, Provider};
use crate::env_table::EnvSnapshot;
use crate::secrets::Secrets;

/// What a live key check learned, beyond "accepted".
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct KeyReport {
    /// A human label for the key when the provider names one (OpenRouter's key
    /// label); absent otherwise.
    pub label: Option<String>,
}

/// What a live model check learned.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelReport {
    pub context_length: u64,
    pub max_output_tokens: Option<u64>,
}

/// One entry of a live model listing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelListing {
    pub id: String,
    pub display_name: String,
}

/// The lookups setup makes. Every network method takes the credential by
/// value for the call and keeps nothing. In-crate callers only, so the
/// futures carry no `Send` bound.
#[allow(async_fn_in_trait)]
pub trait Probe {
    /// The value of an environment variable in the shell setup runs in.
    fn env_var(&self, name: &str) -> Option<String>;
    /// Where a pasted key would be kept (docs/secrets.md): the store's
    /// description, or why there is none to offer.
    fn secret_store(&self) -> Result<String, String>;
    /// Keep `value` under `name` in that store.
    fn store_secret(&self, name: &str, value: &str) -> Result<(), String>;
    /// Whether Codex CLI's cached login is readable: `Ok(account id)` or why not.
    fn codex_cache(&self) -> Result<String, String>;
    /// Whether the provider accepts `key`. `Ok(None)` when the provider has no
    /// route to ask.
    async fn check_key(&self, provider: Provider, key: &str) -> Result<Option<KeyReport>, String>;
    /// The models `key` can see on `provider`, for a menu; empty when the
    /// provider has no short listing to offer.
    async fn list_models(&self, provider: Provider, key: &str)
    -> Result<Vec<ModelListing>, String>;
    /// The provider's metadata for `model`, as the boot path would read it.
    async fn check_model(
        &self,
        provider: Provider,
        key: &str,
        model: &str,
    ) -> Result<ModelReport, String>;
}

/// The real thing: this process's environment, the Codex cache under its
/// `HOME`, and the providers over HTTPS.
pub struct LiveProbe {
    env: EnvSnapshot,
    cwd: std::path::PathBuf,
    /// The store, or why it could not be opened; setup goes on either way.
    secrets: Result<Secrets, String>,
}

impl LiveProbe {
    pub fn capture() -> Result<Self, crate::api::Error> {
        Ok(Self {
            env: EnvSnapshot::capture()?,
            cwd: std::env::current_dir()?,
            secrets: Secrets::detect().map_err(|error| error.to_string()),
        })
    }

    fn endpoint(provider: Provider, key: &str) -> Result<Endpoint, String> {
        Endpoint::from_credentials(provider, key.to_string(), None)
            .map_err(|error| error.to_string())
    }
}

impl Probe for LiveProbe {
    fn env_var(&self, name: &str) -> Option<String> {
        self.env.get(name).map(str::to_string)
    }

    fn secret_store(&self) -> Result<String, String> {
        self.secrets
            .as_ref()
            .map(|secrets| secrets.describe().to_string())
            .map_err(Clone::clone)
    }

    fn store_secret(&self, name: &str, value: &str) -> Result<(), String> {
        let secrets = self.secrets.as_ref().map_err(Clone::clone)?;
        secrets.set(name, value).map_err(|error| error.to_string())
    }

    fn codex_cache(&self) -> Result<String, String> {
        let read = |name: &str| self.env.get(name).map(str::to_string);
        crate::api::codex_cached_credentials(Path::new(&self.cwd), &read)
            .map(|(_, account)| account)
            .map_err(|error| error.to_string())
    }

    async fn check_key(&self, provider: Provider, key: &str) -> Result<Option<KeyReport>, String> {
        let body = Self::endpoint(provider, key)?
            .check_key()
            .await
            .map_err(|error| error.to_string())?;
        Ok(body.map(|body| KeyReport {
            label: openrouter_key_label(&body),
        }))
    }

    async fn list_models(
        &self,
        provider: Provider,
        key: &str,
    ) -> Result<Vec<ModelListing>, String> {
        let models = Self::endpoint(provider, key)?
            .list_models()
            .await
            .map_err(|error| error.to_string())?;
        Ok(models
            .into_iter()
            .map(|(id, display_name)| ModelListing { id, display_name })
            .collect())
    }

    async fn check_model(
        &self,
        provider: Provider,
        key: &str,
        model: &str,
    ) -> Result<ModelReport, String> {
        let capabilities = Self::endpoint(provider, key)?
            .fetch_capabilities(model, &[])
            .await
            .map_err(|error| error.to_string())?;
        Ok(ModelReport {
            context_length: capabilities.context_length,
            max_output_tokens: capabilities.max_output_tokens,
        })
    }
}

/// OpenRouter's `GET /api/v1/key` body carries `data.label`; any other body
/// (another provider's listing) has none.
fn openrouter_key_label(body: &str) -> Option<String> {
    let json: serde_json::Value = serde_json::from_str(body).ok()?;
    json.get("data")?
        .get("label")?
        .as_str()
        .filter(|label| !label.is_empty())
        .map(str::to_string)
}
