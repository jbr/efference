//! The subscription backend's model catalog, read from the route Codex CLI
//! reads (docs/transport.md).
//!
//! This is the capability source for both OpenAI arms: the catalog names each model
//! the backend serves and
//! reports a context window, its reasoning levels, and its input modalities, so
//! the facts a boot path checks come from the provider rather than from the
//! build. The route answers a platform API key as well as a ChatGPT grant, and
//! the platform's own Models API reports none of these fields, so the platform
//! arm reads it too — as a borrowed source, whose failure degrades to the arm's
//! baseline rather than refusing the session.
//!
//! Two properties of the route shape what is read here:
//!
//! - It requires a `client_version` query parameter, and the backend returns
//!   only the entries whose `minimal_client_version` the claim satisfies. What a
//!   client can see therefore follows the version it claims, and a claim below
//!   every entry's minimum answers 200 with an empty catalog rather than an
//!   error — which is why [`CLAIMED_CLIENT_VERSION`] is a claim of its own and
//!   not this crate's version.
//! - An entry carries no output cap, no temperature acceptance, and no cache
//!   policy. Those stay as the arm's baseline asserts (or, for the cap and the
//!   temperature, as nothing asserted), because a source that says nothing is
//!   not a source that says no.
//!
//! A model the catalog does not name is **not** an error: a listing can lag a
//! release, and the id may still be served. The model passes boot on the
//! baseline, and the provider's own answer is the test.

use super::row::ListedModel;
use super::*;

#[cfg(test)]
mod test;

/// The `client_version` this build claims to the catalog route. The backend
/// filters entries by `minimal_client_version`, and the models added most
/// recently are gated above the version of any client the operator has
/// installed — the claim is what makes them visible, so it names the newest
/// generation rather than ours. Sending this crate's own version instead
/// returns an empty catalog, which reads as "this account serves no models"
/// rather than as a wrong claim.
pub(super) const CLAIMED_CLIENT_VERSION: &str = "99.99.99";

/// How long a catalog read stands before the next one asks again, matching the
/// interval Codex CLI caches its own copy over. A catalog changes when OpenAI
/// ships a model, not within a session's life.
const CATALOG_TTL: std::time::Duration = std::time::Duration::from_secs(300);

/// The ceiling on one catalog read. It is on the boot path, so a slow route
/// fails the lookup rather than hanging a session's creation — the same
/// treatment the boot metadata read gets.
const CATALOG_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// One entry of the catalog: what the backend says about one model.
#[derive(Debug, Clone, Deserialize)]
pub(super) struct Entry {
    /// The provider-native model id, the key a turn names.
    pub slug: String,
    #[serde(default)]
    pub display_name: String,
    #[serde(default)]
    pub description: Option<String>,
    /// The window the model works in. `max_context_window` is the ceiling a
    /// configuration override may raise it to, so it stands in only when the
    /// working window is absent.
    #[serde(default)]
    pub context_window: Option<u64>,
    #[serde(default)]
    pub max_context_window: Option<u64>,
    #[serde(default)]
    pub input_modalities: Vec<String>,
    /// The reasoning levels the model accepts, each an object naming its
    /// `effort` and a description the harness does not read.
    #[serde(default)]
    pub supported_reasoning_levels: Vec<Level>,
    #[serde(default)]
    pub default_reasoning_level: Option<String>,
    /// The backend's menu choice for this entry: `list` shows it, `hide` does
    /// not. Which is a display decision, not an access boundary — a hidden
    /// entry serves turns like any other, so it is excluded from suggestions
    /// and nothing more.
    #[serde(default)]
    pub visibility: Option<String>,
    /// Whether the backend serves this entry through the API surface. Absent
    /// asserts nothing, as everywhere else here.
    #[serde(default)]
    pub supported_in_api: Option<bool>,
}

#[derive(Debug, Clone, Deserialize)]
pub(super) struct Level {
    #[serde(default)]
    pub effort: String,
}

impl Entry {
    /// This entry as a menu row. An entry that lists no modality or no reasoning
    /// level declares nothing about either, so those facts stay absent.
    fn row(&self) -> ListedModel {
        ListedModel {
            context: self.context_window.or(self.max_context_window),
            reasoning: self.efforts().map(|_| true),
            images: (!self.input_modalities.is_empty())
                .then(|| self.input_modalities.iter().any(|m| m == "image")),
            ..ListedModel::new(&self.slug)
                .named(&self.display_name)
                .described(self.description.as_deref())
        }
    }

    /// Whether this entry belongs in a menu of models to choose from.
    pub(super) fn is_suggested(&self) -> bool {
        self.visibility.as_deref() != Some("hide") && self.supported_in_api != Some(false)
    }

    /// The reasoning levels this entry declares, in the order it lists them.
    /// Empty when it declares none — and an entry that declares none does not
    /// validate a configured level, since nothing affirmed any.
    pub(super) fn efforts(&self) -> Option<Vec<String>> {
        let levels: Vec<String> = self
            .supported_reasoning_levels
            .iter()
            .map(|level| level.effort.clone())
            .filter(|effort| !effort.is_empty())
            .collect();
        (!levels.is_empty()).then_some(levels)
    }

    /// What this entry reports about the model, over the arm's baseline: the
    /// fields the catalog speaks for are filled in, and the ones it does not
    /// keep whatever the baseline asserted.
    pub(super) fn capabilities(&self, baseline: ModelCapabilities) -> ModelCapabilities {
        let efforts = self.efforts();
        ModelCapabilities {
            context_length: self.context_window.or(self.max_context_window),
            supports_reasoning: efforts.is_some(),
            // An entry that lists no modality accepts no image: the upload would
            // spend itself on a rejection, and the tool reports success either
            // way (docs/tools.md).
            supports_image_input: self.input_modalities.iter().any(|m| m == "image"),
            supported_efforts: efforts.clone(),
            default_effort: self.default_reasoning_level.clone(),
            // The levels are reported, not enforced. What an entry lists is the
            // set Codex CLI offers, which is not the set the Responses API
            // accepts: `ultra` is described as "automatic task delegation" and
            // answers 400 `Invalid value: 'ultra'` on a `gpt-6-sol` turn, while
            // `none` and `minimal` are accepted without being listed at all.
            // Validating against that list would refuse two values the provider
            // takes and wave through one it refuses, so the provider's own
            // answer is the check — and it names every value it accepts, which
            // no list in this build can.
            validates_efforts: false,
            ..baseline
        }
    }
}

/// What an arm asserts before any entry is read: the cache policy, and nothing
/// else. The catalog reports no cache policy of its own, so this is what its
/// entries are read over.
///
/// The Codex backend rejects the whole `gpt-5.6+` cache-control family —
/// `prompt_cache_options` and `prompt_cache_retention` both answer 400
/// "Unsupported parameter" on every model it serves, so this is the backend's
/// surface rather than one model's. Explicit mode is therefore unreachable
/// there, and would be worse than unreachable if half-applied: `mode: explicit`
/// with no breakpoints disables caching outright. On that arm the provider
/// places the breakpoints and the encoder emits no markers; the sticky key
/// ([`Endpoint::session_id`]) is what makes them land.
///
/// The platform accepts explicit breakpoints: a repeated one-shot call on
/// `gpt-6-luna` wrote 5,974 cache tokens and read the same 5,974 back
/// (2026-09-25).
pub(super) fn baseline(provider: Provider) -> ModelCapabilities {
    let prompt_cache = match provider {
        Provider::OpenAICodex => PromptCachePolicy::Implicit,
        _ => PromptCachePolicy::Explicit,
    };
    ModelCapabilities {
        prompt_cache,
        ..ModelCapabilities::unknown()
    }
}

/// One model's capabilities on either OpenAI arm: its catalog entry over the
/// arm's baseline, or the baseline alone for a model the catalog does not name.
///
/// A catalog that cannot be read is the caller's failure on the Codex arm, where
/// it is the arm's own source. On the platform arm it is a borrowed one — an
/// undocumented route that happens to answer a platform key — so its failure is
/// reported and the baseline stands, the same answer a model absent from the
/// catalog gets.
pub(super) async fn capabilities(
    endpoint: &Endpoint,
    model: &str,
) -> Result<ModelCapabilities, Error> {
    let baseline = baseline(endpoint.provider);
    let catalog = match catalog(endpoint).await {
        Ok(catalog) => catalog,
        Err(error) if endpoint.provider == Provider::OpenAI => {
            log::warn!(
                "the openai model catalog could not be read ({error}); {model} boots on \
                 the platform baseline, with no context window"
            );
            return Ok(baseline);
        }
        Err(error) => return Err(error),
    };
    Ok(match catalog.entry(model) {
        Some(entry) => entry.capabilities(baseline),
        None => baseline,
    })
}

/// One catalog read: the entries in the order the backend listed them.
#[derive(Debug, Clone, Deserialize)]
pub(super) struct Listing {
    // No `default`: the key is how a body says it is a catalog at all. An
    // absent one is a refusal or some other document, and reading it as an
    // empty catalog would claim the account serves no models.
    models: Vec<Entry>,
}

impl Listing {
    /// The entry for `model`, or `None` when the catalog does not name it.
    pub(super) fn entry(&self, model: &str) -> Option<&Entry> {
        self.models.iter().find(|entry| entry.slug == model)
    }

    /// The rows a menu offers, in the backend's order: every entry it does not
    /// hide. The catalog dates nothing, so the order is the backend's own.
    /// Additive — an id absent here is still sent to a turn unchanged.
    pub(super) fn rows(&self) -> Vec<ListedModel> {
        self.models
            .iter()
            .filter(|entry| entry.is_suggested())
            .map(Entry::row)
            .collect()
    }
}

fn parse(body: &str) -> Result<Listing, Error> {
    serde_json::from_str(body)
        .map_err(|e| -> Error { format!("model catalog is not a model catalog: {e}").into() })
}

/// A catalog this process has read, and when.
#[derive(Debug, Clone)]
struct Cached {
    body: String,
    read_at: std::time::Instant,
}

impl Cached {
    fn is_fresh(&self, now: std::time::Instant) -> bool {
        now.duration_since(self.read_at) < CATALOG_TTL
    }
}

/// The catalogs read in this process, by connection. The key is the connection's
/// identity — its provider, base, and account — never its credential, and the map
/// is in-process state a host holds for its own connections (docs/transport.md).
fn cache() -> &'static std::sync::Mutex<std::collections::HashMap<String, Cached>> {
    static CACHE: std::sync::OnceLock<std::sync::Mutex<std::collections::HashMap<String, Cached>>> =
        std::sync::OnceLock::new();
    CACHE.get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()))
}

/// What identifies one connection's catalog: everything but the secret.
fn cache_key(endpoint: &Endpoint) -> String {
    let account = endpoint
        .extra_headers
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case("ChatGPT-Account-ID"))
        .map(|(_, value)| value.as_str())
        .unwrap_or_default();
    cache_key_parts(endpoint.provider, endpoint.base.as_deref(), account)
}

fn cache_key_parts(provider: Provider, base: Option<&str>, account: &str) -> String {
    format!("{provider:?}|{base:?}|{account}")
}

/// Install a catalog as though a read had just happened. Test-only: production
/// reads the route, and a test that needs the boot path to resolve a Codex model
/// without a provider seeds the cache the lookup reads — the same cache, keyed by
/// the connection's identity, so a test that names its own account seeds its own.
#[cfg(test)]
pub(super) fn seed(provider: Provider, base: Option<&str>, account: &str, body: &str) {
    if let Ok(mut cache) = cache().lock() {
        cache.insert(
            cache_key_parts(provider, base, account),
            Cached {
                body: body.to_string(),
                read_at: std::time::Instant::now(),
            },
        );
    }
}

/// The catalog for one connection, read through the process cache: within the
/// TTL the last read stands, and a read that fails falls back to an older one
/// rather than failing the lookup that asked — a catalog that has gone stale is
/// still the provider's last word about which models exist, and the turn is the
/// real test. With nothing cached, the read's failure is the caller's.
pub(super) async fn catalog(endpoint: &Endpoint) -> Result<Listing, Error> {
    let key = cache_key(endpoint);
    let cached = cache()
        .lock()
        .ok()
        .and_then(|cache| cache.get(&key).cloned());
    if let Some(cached) = &cached
        && cached.is_fresh(std::time::Instant::now())
    {
        return parse(&cached.body);
    }
    // The subscription backend's route whichever arm is asking: the platform
    // arm's own listing carries none of these fields.
    let url = Provider::OpenAICodex
        .models_url(None)
        .ok_or("the openai-codex backend has no model catalog route")?;
    let read = tokio::time::timeout(CATALOG_TIMEOUT, endpoint.probe(&url)).await;
    match read {
        Ok(Ok(response))
            if response
                .status
                .is_some_and(|status| (200..300).contains(&status)) =>
        {
            // A body that does not parse is not cached: the next lookup should
            // see the provider's current answer rather than this one's shape.
            let catalog = parse(&response.body)?;
            if let Ok(mut cache) = cache().lock() {
                cache.insert(
                    key,
                    Cached {
                        body: response.body,
                        read_at: std::time::Instant::now(),
                    },
                );
            }
            Ok(catalog)
        }
        // A refusal is a fact about the route rather than a transport failure,
        // and a body that says why is worth more than the status alone.
        Ok(Ok(response)) => {
            let status = response
                .status
                .map_or_else(|| "no status".to_string(), |status| status.to_string());
            stale(
                cached.as_ref(),
                endpoint,
                format!(
                    "model catalog at {url} returned {status}: {}",
                    response.body
                )
                .into(),
            )
        }
        Ok(Err(error)) => stale(
            cached.as_ref(),
            endpoint,
            format!("model catalog at {url} could not be read: {error}").into(),
        ),
        Err(_elapsed) => stale(
            cached.as_ref(),
            endpoint,
            format!("model catalog at {url} did not answer in time").into(),
        ),
    }
}

/// Serve a cached catalog after a failed refresh, saying so on the way out: a
/// stale answer used silently is an answer nobody knows to distrust. With
/// nothing cached, the failure that stopped the refresh is the caller's.
fn stale(cached: Option<&Cached>, endpoint: &Endpoint, error: Error) -> Result<Listing, Error> {
    let Some(cached) = cached else {
        return Err(error);
    };
    let age = std::time::Instant::now()
        .duration_since(cached.read_at)
        .as_secs();
    log::warn!(
        "the {} model catalog is {age}s old and its refresh failed ({error}); using it",
        endpoint.provider
    );
    parse(&cached.body)
}
