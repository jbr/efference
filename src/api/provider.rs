//! Provider identity and the model-metadata (capability) query it answers
//! (docs/transport.md).
//!
//! The provider is **declared** via `EFFERENCE_PROVIDER`, never inferred from a
//! URL shape (sniffing fails for proxies and custom URLs). It is a closed enum,
//! grown in code as providers are added — an externally-defined provider would
//! have to supply everything a variant supplies (endpoint, auth shape,
//! capability source, a known-models map), and that interface is deferred until
//! the in-code providers show what it must carry.

use super::*;

/// Which API family fronts the messages endpoint. Each variant knows its
/// messages URL, its auth header shape, and its model-metadata endpoint — the
/// operator declares one word and everything else is derived.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Provider {
    /// Anthropic's own API: `x-api-key` auth, metadata at `GET /v1/models/{model}`.
    Anthropic,
    /// OpenRouter's Anthropic-Messages skin (`/api/v1/messages`): `authorization:
    /// Bearer` auth, metadata at `GET /api/v1/models/{author}/{slug}/endpoints`.
    OpenRouter,
}

impl Provider {
    /// Parse the `EFFERENCE_PROVIDER` value. Unset defaults to [`Anthropic`];
    /// anything unrecognized is a hard error naming the valid values — never a
    /// silent fallback (a wrong guess would send the key to the wrong host).
    ///
    /// [`Anthropic`]: Self::Anthropic
    pub(super) fn parse(value: Option<&str>) -> Result<Self, Error> {
        match value.map(str::trim) {
            None | Some("") => Ok(Self::Anthropic),
            Some("anthropic") => Ok(Self::Anthropic),
            Some("openrouter") => Ok(Self::OpenRouter),
            Some(other) => Err(format!(
                "EFFERENCE_PROVIDER={other:?} not recognized: valid values are \
                 `anthropic` and `openrouter` (a new provider is a new enum \
                 variant — docs/transport.md)"
            )
            .into()),
        }
    }

    /// The lowercase name, as `EFFERENCE_PROVIDER` spells it — for error
    /// messages and boot output.
    pub fn name(&self) -> &'static str {
        match self {
            Self::Anthropic => "anthropic",
            Self::OpenRouter => "openrouter",
        }
    }

    /// The messages endpoint this provider serves the Anthropic wire on.
    pub(super) fn messages_url(&self) -> &'static str {
        match self {
            Self::Anthropic => "https://api.anthropic.com/v1/messages",
            Self::OpenRouter => "https://openrouter.ai/api/v1/messages",
        }
    }

    /// The auth header **name** this provider reads.
    pub(super) fn auth_header(&self) -> &'static str {
        match self {
            Self::Anthropic => "x-api-key",
            Self::OpenRouter => "authorization",
        }
    }

    /// The literal prepended to the secret to form the auth header value.
    /// Split off the secret so the secret stays a *whole* value (an `op://…`
    /// reference `op run` can substitute — docs/transport.md).
    pub(super) fn auth_prefix(&self) -> &'static str {
        match self {
            Self::Anthropic => "",
            Self::OpenRouter => "Bearer ",
        }
    }

    /// The model-metadata URL for `model`. OpenRouter model ids are
    /// `author/slug`, which its metadata route takes as two path segments; a
    /// slash-less id can't be queried and errors here, before any request.
    pub(super) fn capabilities_url(&self, model: &str) -> Result<String, Error> {
        match self {
            Self::Anthropic => Ok(format!("https://api.anthropic.com/v1/models/{model}")),
            Self::OpenRouter => {
                let (author, slug) = model.split_once('/').ok_or_else(|| -> Error {
                    format!(
                        "OpenRouter model ids are `author/slug` (e.g. \
                         `anthropic/claude-haiku-4.5`); {model:?} has no author segment"
                    )
                    .into()
                })?;
                Ok(format!(
                    "https://openrouter.ai/api/v1/models/{author}/{slug}/endpoints"
                ))
            }
        }
    }

    /// Parse this provider's metadata response into [`ModelCapabilities`]. A
    /// response missing the fields we need is an error (the boot step is
    /// required — docs/transport.md), never a silent default.
    pub(super) fn parse_capabilities(&self, body: &str) -> Result<ModelCapabilities, Error> {
        match self {
            Self::Anthropic => {
                // `GET /v1/models/{id}`: `max_input_tokens` is the context
                // window, `max_tokens` the output cap (both carried since
                // 2026-03). Unknown fields (`capabilities`, …) are ignored
                // until a consumer needs them.
                #[derive(Deserialize)]
                struct Model {
                    max_input_tokens: Option<u64>,
                    max_tokens: Option<u64>,
                }
                let model: Model = serde_json::from_str(body)?;
                let context_length = model.max_input_tokens.ok_or_else(|| -> Error {
                    "Anthropic model metadata carried no max_input_tokens".into()
                })?;
                Ok(ModelCapabilities {
                    context_length,
                    max_output_tokens: model.max_tokens,
                })
            }
            Self::OpenRouter => {
                // `GET /api/v1/models/{author}/{slug}/endpoints`: one entry per
                // upstream currently serving the model, each with its own
                // `context_length` / `max_completion_tokens`. Routing is
                // unpinned, so the effective ceiling is the **min** across
                // endpoints — the conservative bound until provider pinning
                // lands. Model-level fields (`architecture.input_modalities`,
                // per-endpoint pricing) are ignored until a consumer needs them.
                #[derive(Deserialize)]
                struct Body {
                    data: Data,
                }
                #[derive(Deserialize)]
                struct Data {
                    endpoints: Vec<ModelEndpoint>,
                }
                #[derive(Deserialize)]
                struct ModelEndpoint {
                    context_length: Option<u64>,
                    max_completion_tokens: Option<u64>,
                }
                let body: Body = serde_json::from_str(body)?;
                let context_length = body
                    .data
                    .endpoints
                    .iter()
                    .filter_map(|e| e.context_length)
                    .min()
                    .ok_or_else(|| -> Error {
                        "OpenRouter model metadata listed no endpoint with a context_length \
                         (is the model currently served?)"
                            .into()
                    })?;
                let max_output_tokens = body
                    .data
                    .endpoints
                    .iter()
                    .filter_map(|e| e.max_completion_tokens)
                    .min();
                Ok(ModelCapabilities {
                    context_length,
                    max_output_tokens,
                })
            }
        }
    }
}

/// What the provider's model-metadata endpoint reports about the selected
/// model. Fetched once at boot — a **required** step; `serve` aborts if the
/// lookup fails (docs/transport.md) — held in memory, transport-only: never
/// logged or projected.
///
/// Grows one field per capability consumer: a new conditional (image input,
/// implicit-caching support, pricing) is a field here plus its extraction from
/// each provider's response shape.
#[derive(Debug, Clone, Serialize)]
pub struct ModelCapabilities {
    /// Maximum prompt tokens the model accepts. Anthropic `max_input_tokens`;
    /// OpenRouter: the min `context_length` across the endpoints currently
    /// serving the model.
    pub context_length: u64,
    /// Maximum response tokens, where the provider reports one.
    pub max_output_tokens: Option<u64>,
}

impl Endpoint {
    /// The required boot-time metadata lookup (docs/transport.md): GET the
    /// provider's model-metadata endpoint for `model` and parse it into
    /// [`ModelCapabilities`]. Authenticated with the same header as the
    /// messages POSTs (Anthropic's Models API requires the key; OpenRouter
    /// tolerates it). Any failure — network, non-2xx, missing fields — is an
    /// error the caller treats as fatal to startup.
    pub async fn fetch_capabilities(&self, model: &str) -> Result<ModelCapabilities, Error> {
        let url = self.provider.capabilities_url(model)?;
        let body = self.authed_get(&url).await?;
        self.provider
            .parse_capabilities(&body)
            .map_err(|e| -> Error {
                format!("could not parse model metadata from {url}: {e}").into()
            })
    }

    /// One request's billed cost in USD, from the provider's own accounting —
    /// authoritative, never modeled from a rate card (cache read/write/TTL
    /// multipliers make modeling brittle; docs/transport.md). OpenRouter keys
    /// it by the generation id every response carries — the same id
    /// `usage.jsonl` already records per request. Anthropic has no
    /// per-request cost surface, so the daemon never calls this there.
    pub async fn fetch_generation_cost(&self, id: &str) -> Result<f64, Error> {
        let url = match self.provider {
            Provider::OpenRouter => {
                format!("https://openrouter.ai/api/v1/generation?id={id}")
            }
            Provider::Anthropic => {
                return Err("anthropic exposes no per-generation cost endpoint".into());
            }
        };
        parse_generation_cost(&self.authed_get(&url).await?)
    }

    /// One authenticated GET against a provider metadata/accounting URL,
    /// returning the body on 2xx and a status+body error otherwise.
    async fn authed_get(&self, url: &str) -> Result<String, Error> {
        let http = HttpClient::new(RustlsConfig::<ClientConfig>::default());
        let mut conn = http
            .get(url)
            .with_request_header(self.auth_header.clone(), self.auth_value.clone())
            .with_request_header("anthropic-version", ANTHROPIC_VERSION)
            .await
            .map_err(|e| -> Error { format!("GET {url} failed: {e}").into() })?;
        let status = conn.status();
        let body = conn.response_body().read_string().await?;
        if !status.is_some_and(|s| s.is_success()) {
            let status = status.map_or_else(|| "no status".to_string(), |s| s.to_string());
            return Err(format!("GET {url} returned {status}: {body}").into());
        }
        Ok(body)
    }
}

/// Parse OpenRouter's `GET /api/v1/generation` response down to `total_cost` —
/// what the account was actually billed for one request. (`upstream_inference_cost`
/// is BYOK-only and stays ignored.) A record without the field is an error the
/// caller counts as an unpriced request, never a silent zero.
pub(super) fn parse_generation_cost(body: &str) -> Result<f64, Error> {
    #[derive(Deserialize)]
    struct Body {
        data: Generation,
    }
    #[derive(Deserialize)]
    struct Generation {
        total_cost: Option<f64>,
    }
    serde_json::from_str::<Body>(body)?
        .data
        .total_cost
        .ok_or_else(|| "generation record carried no total_cost".into())
}
