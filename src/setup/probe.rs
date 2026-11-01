//! How setup looks at the world: the environment it runs in, the Codex cache, and
//! the providers themselves (docs/operating.md). The flow only ever sees the
//! trait, so a test answers from a table and never opens a socket.

use std::path::Path;

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;

use crate::api::Endpoint;
use crate::env_table::EnvSnapshot;
use crate::platform::secrets::Secrets;
use crate::record::log::Provider;

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
    pub context_length: Option<u64>,
    pub max_output_tokens: Option<u64>,
}

/// What a completed ChatGPT login learned, once the grant is stored
/// (docs/secrets.md). The tokens stay in the store; nothing the flow prints
/// comes from them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatGptLogin {
    /// `chatgpt_account_id`, which every request on this connection carries.
    pub account_id: String,
    /// The plan the grant named at login, when it named one.
    pub plan: Option<String>,
}

/// One entry of a live model listing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelListing {
    pub id: String,
    pub display_name: String,
}

/// What a probe asks about: the provider, plus the base URL a described endpoint
/// carries and a named one does not (docs/transport.md).
///
/// One value rather than a second parameter on each method, so adding what a
/// connection knows about itself is one struct field instead of an edit at every
/// call site and in both implementations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    pub provider: Provider,
    pub base: Option<String>,
}

impl Target {
    /// A provider this build knows by name, which owns its own URLs.
    pub fn named(provider: Provider) -> Self {
        Self {
            provider,
            base: None,
        }
    }

    /// An endpoint described by dialect and base URL.
    pub fn described(dialect: crate::record::log::WireDialect, base: impl Into<String>) -> Self {
        Self {
            provider: Provider::Custom(dialect),
            base: Some(base.into()),
        }
    }
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
    /// Whether Codex CLI's cached login is readable: `Ok(account id)` or why not
    /// — including a token past its own `exp` claim, decoded locally with no
    /// network call.
    fn codex_cache(&self) -> Result<String, String>;
    /// Run the ChatGPT authorization dance and store the grant under
    /// `oauth:<connection>` (docs/secrets.md), returning what it named. The
    /// browser opening and the HTTP round trips live behind this method so the
    /// flow stays testable. `say` is how the person is told where to log in: the
    /// flow's prompter, so the lines land in the transcript it keeps.
    async fn chatgpt_login(
        &self,
        connection: &str,
        say: &mut dyn FnMut(&str),
    ) -> Result<ChatGptLogin, String>;
    /// The value kept under `name` in the store, when there is a store and it
    /// holds one. Read-only: never printed except by an explicit reveal.
    fn secret_value(&self, name: &str) -> Result<Option<String>, String>;
    /// Remove `name` from the store. Absent is not an error.
    fn delete_secret(&self, name: &str) -> Result<(), String>;
    /// Whether the provider accepts `key`. `Ok(None)` when the provider has no
    /// route to ask.
    async fn check_key(&self, target: &Target, key: &str) -> Result<Option<KeyReport>, String>;
    /// The models `key` can see there, for a menu; empty when the endpoint has no
    /// short listing to offer.
    async fn list_models(&self, target: &Target, key: &str) -> Result<Vec<ModelListing>, String>;
    /// The metadata for `model`, as the boot path would read it.
    async fn check_model(
        &self,
        target: &Target,
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
    /// The probe for a run that edits the configuration `config_text`, whose
    /// `[secrets]` table names the store a pasted key or a sign-in goes to.
    pub fn capture(config_text: &str) -> Result<Self, crate::api::Error> {
        Ok(Self {
            env: EnvSnapshot::capture()?,
            cwd: std::env::current_dir()?,
            // Opened here rather than by the first write, which is the
            // default (docs/secrets.md): setup offers to keep a value only
            // where keeping one can work, and says why not otherwise.
            secrets: crate::user_config::InferenceConfig::from_toml(config_text)
                .map_err(|error| error.to_string())
                .map(|config| Secrets::configured(config.secrets))
                .and_then(|secrets| secrets.open().map(|()| secrets)),
        })
    }

    fn endpoint(target: &Target, key: &str) -> Result<Endpoint, String> {
        Endpoint::from_credentials(target.provider, key, None, target.base.clone())
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

    fn secret_value(&self, name: &str) -> Result<Option<String>, String> {
        let secrets = self.secrets.as_ref().map_err(Clone::clone)?;
        secrets.get(name).map_err(|error| error.to_string())
    }

    fn delete_secret(&self, name: &str) -> Result<(), String> {
        let secrets = self.secrets.as_ref().map_err(Clone::clone)?;
        secrets.delete(name).map_err(|error| error.to_string())
    }

    fn codex_cache(&self) -> Result<String, String> {
        let read = |name: &str| self.env.get(name).map(str::to_string);
        let (token, account) = crate::api::codex_cached_credentials(Path::new(&self.cwd), &read)
            .map_err(|error| error.to_string())?;
        if let Some(expiry) = jwt_exp(&token)
            && expiry <= now_unix()
        {
            return Err("token expired".to_string());
        }
        Ok(account)
    }

    async fn chatgpt_login(
        &self,
        connection: &str,
        say: &mut dyn FnMut(&str),
    ) -> Result<ChatGptLogin, String> {
        let secrets = self.secrets.as_ref().map_err(Clone::clone)?;
        let grant = crate::inference::chatgpt::log_in(|url| {
            present_authorization(url, say);
            Ok(())
        })
        .await?;
        grant.save(secrets, connection)?;
        Ok(ChatGptLogin {
            account_id: grant.account_id,
            plan: grant.plan,
        })
    }

    async fn check_key(&self, target: &Target, key: &str) -> Result<Option<KeyReport>, String> {
        let body = Self::endpoint(target, key)?
            .check_key()
            .await
            .map_err(|error| error.to_string())?;
        Ok(body.map(|body| KeyReport {
            label: openrouter_key_label(&body),
        }))
    }

    async fn list_models(&self, target: &Target, key: &str) -> Result<Vec<ModelListing>, String> {
        let models = Self::endpoint(target, key)?
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
        target: &Target,
        key: &str,
        model: &str,
    ) -> Result<ModelReport, String> {
        let capabilities = Self::endpoint(target, key)?
            .fetch_capabilities(model, &[])
            .await
            .map_err(|error| error.to_string())?;
        Ok(ModelReport {
            context_length: capabilities.context_length,
            max_output_tokens: capabilities.max_output_tokens,
        })
    }
}

/// Get the person to the authorization URL: say it, then try to open it.
///
/// The URL is said either way, so the open is attempted unconditionally and
/// a failure is reported rather than fatal — the same reasoning as the MCP
/// authorization path (`src/boot/mcp_mount.rs`). Said through the flow's
/// prompter, so it reads in place in the transcript setup keeps.
fn present_authorization(url: &str, say: &mut dyn FnMut(&str)) {
    say(&format!(
        "Log in to ChatGPT to authorize efference:\n\n  {url}\n"
    ));
    match webbrowser::open(url) {
        Ok(()) => say("(opened in your browser — waiting for you to finish)"),
        Err(error) => say(&format!(
            "(could not open a browser: {error} — open the URL above to continue)"
        )),
    }
}

/// The `exp` claim of a JWT's payload segment, undecorated by a signature
/// check — this is a local readiness hint, never the credential a request
/// carries, so a token that fails to parse is simply not flagged expired.
fn jwt_exp(token: &str) -> Option<i64> {
    let payload = token.split('.').nth(1)?;
    let bytes = URL_SAFE_NO_PAD.decode(payload).ok()?;
    let json: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    json.get("exp")?.as_i64()
}

fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs() as i64)
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
