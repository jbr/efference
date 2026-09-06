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

mod openai;

pub use openai::model_capabilities as openai_model_capabilities;

/// The request/response shape a provider speaks (docs/transport.md). Derived
/// from the declared provider — there is no independent knob — so one session
/// projects and assembles in exactly one dialect.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WireDialect {
    /// Anthropic Messages: `system` + `messages[]`, content blocks, SSE
    /// `message_start` … `message_stop`.
    Messages,
    /// OpenAI Responses: `input[]` items, ordered output items, SSE
    /// `response.created` … `response.completed`.
    Responses,
}

impl WireDialect {
    /// The longest tool name this dialect's request accepts. Both dialects share
    /// the alphabet `[a-zA-Z0-9_-]`; only the ceiling differs — Messages allows 128
    /// characters, Responses 64 (docs/tools.md).
    pub fn tool_name_limit(self) -> usize {
        match self {
            Self::Messages => 128,
            Self::Responses => 64,
        }
    }

    /// Why `name` is not a legal tool name on this dialect, or `None` if it is. The
    /// check runs over the frozen toolbox at boot (docs/tools.md), so an illegal name
    /// fails the session before a request carries it and the provider answers 400.
    pub fn tool_name_problem(self, name: &str) -> Option<String> {
        let limit = self.tool_name_limit();
        if name.is_empty() {
            return Some("is empty".into());
        }
        if name.len() > limit {
            return Some(format!(
                "is {} characters; the {self} dialect allows at most {limit}",
                name.len()
            ));
        }
        name.chars()
            .find(|c| !(c.is_ascii_alphanumeric() || *c == '_' || *c == '-'))
            .map(|c| format!("contains {c:?}; the {self} dialect allows only [a-zA-Z0-9_-]"))
    }
}

impl std::fmt::Display for WireDialect {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Messages => "Messages",
            Self::Responses => "Responses",
        })
    }
}

/// Which API family fronts the messages endpoint. Each variant knows its
/// messages URL, its auth header shape, and its model-metadata endpoint — the
/// operator declares one word and everything else is derived.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Provider {
    /// Anthropic's own API: `x-api-key` auth, metadata at `GET /v1/models/{model}`.
    Anthropic,
    /// OpenRouter's Anthropic-Messages skin (`/api/v1/messages`): `authorization:
    /// Bearer` auth, metadata at `GET /api/v1/models/{author}/{slug}/endpoints`
    /// (ceilings) plus `GET /api/v1/model/{author}/{slug}` (effort levels).
    OpenRouter,
    /// OpenAI's Responses API (`/v1/responses`): `authorization: Bearer` auth,
    /// capabilities from the compiled catalog in `provider/openai.rs` because
    /// the Models endpoint reports none of the limits the boot path consumes.
    OpenAI,
    /// The Codex backend (`chatgpt.com/backend-api/codex/responses`): the same
    /// Responses wire, billed to a ChatGPT/Codex subscription instead of the
    /// platform balance. Auth is the OAuth access token Codex CLI caches in
    /// `~/.codex/auth.json` plus its `ChatGPT-Account-ID`; the request shape
    /// mirrors what Codex CLI sends ([`ResponsesProfile::Codex`]).
    OpenAICodex,
}

/// Which Responses request shape a provider takes (docs/transport.md). The
/// platform endpoint documents the full parameter set; the Codex backend is
/// undocumented, so that arm sends only what Codex CLI itself sends.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResponsesProfile {
    Platform,
    Codex,
}

impl Provider {
    /// Whether a configured effort level is validated against the metadata's
    /// declared levels at boot (docs/transport.md). OpenRouter: yes — the
    /// breadth of models behind one slug and the silent nearest-mapping of an
    /// unsupported level are the misconfiguration surface, and its metadata
    /// affirmatively declares the levels. Anthropic: no — its metadata names
    /// no levels, and the provider's own 400 on the first turn is the
    /// feedback for a misspelled one.
    pub fn validates_effort_levels(&self) -> bool {
        matches!(self, Self::OpenRouter | Self::OpenAI | Self::OpenAICodex)
    }

    /// The wire dialect this provider speaks (docs/transport.md).
    pub fn dialect(&self) -> WireDialect {
        match self {
            Self::Anthropic | Self::OpenRouter => WireDialect::Messages,
            Self::OpenAI | Self::OpenAICodex => WireDialect::Responses,
        }
    }

    /// The Responses request shape, for providers on that dialect.
    pub fn responses_profile(&self) -> ResponsesProfile {
        match self {
            Self::OpenAICodex => ResponsesProfile::Codex,
            Self::Anthropic | Self::OpenRouter | Self::OpenAI => ResponsesProfile::Platform,
        }
    }

    /// Whether this provider's capabilities come from the compiled OpenAI catalog.
    pub fn uses_openai_catalog(&self) -> bool {
        matches!(self, Self::OpenAI | Self::OpenAICodex)
    }

    /// The spotter model a bare `serve` uses on this provider
    /// (docs/permissions.md): the cheap band of whatever catalog the provider
    /// serves, since the spotter shares the endpoint and a Haiku id is meaningless
    /// on OpenAI. An explicit `--spotter-model` replaces it and still passes the
    /// provider's capability check.
    pub fn default_spotter_model(&self) -> &'static str {
        if self.uses_openai_catalog() {
            openai::CHEAPEST_MODEL
        } else {
            "claude-haiku-4-5"
        }
    }

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
            Some("openai") => Ok(Self::OpenAI),
            Some("openai-codex" | "codex") => Ok(Self::OpenAICodex),
            Some(other) => Err(format!(
                "EFFERENCE_PROVIDER={other:?} not recognized: valid values are \
                 `anthropic`, `openrouter`, `openai`, and `openai-codex` (a new provider \
                 is a new enum variant — docs/transport.md)"
            )
            .into()),
        }
    }

    /// Where projection places a tool result's images for this provider and model
    /// (docs/log-and-projection.md): the Anthropic-native in-`tool_result` shape
    /// on the native API; the portable following-user-block shape on OpenRouter,
    /// whose bridge drops `tool_result` images for some upstreams (its docs cover
    /// images as user content only on the Anthropic skin).
    ///
    /// Gemini slugs take the third shape. An image anywhere between a signed
    /// `tool_use` and the next assistant turn makes Gemini reject the replayed
    /// thought signature, so its images need the interposed turn
    /// ([`ImagePlacement::AfterInterposedTurn`], docs/transport.md). The model is
    /// part of the choice because the failure is the upstream's, not the skin's —
    /// it reproduces on the provider's OpenAI-shaped endpoint too — so it follows
    /// the slug rather than the provider.
    pub fn image_placement(&self, model: &str) -> crate::project::ImagePlacement {
        match self {
            Self::Anthropic | Self::OpenAI | Self::OpenAICodex => {
                crate::project::ImagePlacement::InToolResult
            }
            Self::OpenRouter if is_gemini(model) => {
                crate::project::ImagePlacement::AfterInterposedTurn
            }
            Self::OpenRouter => crate::project::ImagePlacement::FollowingUserBlock,
        }
    }

    /// Which block this provider's cache breakpoint attaches to
    /// (docs/log-and-projection.md). Anthropic accepts `cache_control` on any
    /// block, so the marker sits at the true wire tail; OpenRouter translates the
    /// marker for the upstream serving the slug, and a non-text carrier is the
    /// shape that translation does not survive on OpenAI-ish endpoints.
    ///
    /// Fixed at boot from the provider, exactly like [`Self::image_placement`],
    /// so projection-plus-placement is deterministic within a session.
    pub fn breakpoint_placement(&self) -> crate::project::BreakpointPlacement {
        match self {
            // Responses places its own marker (`api/responses.rs`); the Messages
            // placement is unused there.
            Self::Anthropic | Self::OpenAI | Self::OpenAICodex => {
                crate::project::BreakpointPlacement::TrailingBlock
            }
            Self::OpenRouter => crate::project::BreakpointPlacement::LastTextBlock,
        }
    }

    /// The lowercase name, as `EFFERENCE_PROVIDER` spells it — for error
    /// messages and boot output.
    ///
    pub fn name(&self) -> &'static str {
        match self {
            Self::Anthropic => "anthropic",
            Self::OpenRouter => "openrouter",
            Self::OpenAI => "openai",
            Self::OpenAICodex => "openai-codex",
        }
    }
    /// Map the session's 0–100 integer temperature to this provider's native
    /// float range. The integer avoids serialization drift and the two-scale
    /// confusion; `None` means omit the field from the wire entirely, letting
    /// the API use its own default (1.0 everywhere we support).
    ///
    /// - **Anthropic**: `t / 100.0` → 0.0..1.0
    /// - **OpenRouter**, **OpenAI**: `(t / 100.0) * 2.0` → 0.0..2.0
    pub fn map_temperature(&self, t: u8) -> f64 {
        let v = (t as f64) / 100.0;
        match self {
            Self::Anthropic => v,
            Self::OpenRouter | Self::OpenAI | Self::OpenAICodex => v * 2.0,
        }
    }

    /// The messages endpoint this provider serves the Anthropic wire on.
    pub(super) fn messages_url(&self) -> &'static str {
        match self {
            Self::Anthropic => "https://api.anthropic.com/v1/messages",
            Self::OpenRouter => "https://openrouter.ai/api/v1/messages",
            Self::OpenAI => "https://api.openai.com/v1/responses",
            Self::OpenAICodex => "https://chatgpt.com/backend-api/codex/responses",
        }
    }

    /// The auth header **name** this provider reads.
    pub(super) fn auth_header(&self) -> &'static str {
        match self {
            Self::Anthropic => "x-api-key",
            Self::OpenRouter | Self::OpenAI | Self::OpenAICodex => "authorization",
        }
    }

    /// The header this provider reads a sticky-routing session key from, or
    /// `None` where the concept doesn't exist (docs/transport.md). Anthropic has
    /// no equivalent: nothing in Messages selects an upstream, because there is
    /// only one — `metadata.user_id` is an abuse-detection identifier, not a
    /// routing key, and is not a substitute. Gating here rather than sending the
    /// header everywhere means the design never rests on an unrecognized header
    /// being ignored rather than rejected.
    pub(super) fn session_header(&self) -> Option<&'static str> {
        match self {
            Self::Anthropic | Self::OpenAI | Self::OpenAICodex => None,
            Self::OpenRouter => Some("x-session-id"),
        }
    }

    /// The header a sticky-routing token for the user turn in flight travels on,
    /// in both directions (docs/transport.md): the backend sets it on a response,
    /// the client replays it on every later request of the same turn. Only the
    /// Codex backend has this contract; Codex CLI treats it as required. `None`
    /// means neither capture nor replay happens.
    pub(super) fn turn_routing_header(&self) -> Option<&'static str> {
        match self {
            Self::OpenAICodex => Some("x-codex-turn-state"),
            Self::Anthropic | Self::OpenAI | Self::OpenRouter => None,
        }
    }

    /// Whether responses carry the subscription-window headers `api/plan.rs`
    /// reads (docs/transport.md). Only the Codex backend does.
    pub(super) fn reports_plan_windows(&self) -> bool {
        matches!(self, Self::OpenAICodex)
    }

    /// The request header naming the model for the backend's router, sent as
    /// Codex CLI sends it (`model=<slug>`) on the Codex backend only.
    pub(super) fn routing_hint_header(&self) -> Option<&'static str> {
        match self {
            Self::OpenAICodex => Some("x-codex-routing-hint"),
            Self::Anthropic | Self::OpenAI | Self::OpenRouter => None,
        }
    }

    /// The app-attribution headers this provider reads, empty where it has no
    /// such concept (docs/transport.md).
    ///
    /// OpenRouter attributes usage by app rather than by `User-Agent`, keyed on
    /// `HTTP-Referer`; the title and categories only decorate an app the referer
    /// already identified. Sent on the inference POSTs alone — attribution is
    /// about inference, and the metadata GETs in [`Endpoint::authed_get`] are not
    /// billed usage.
    ///
    /// Scoped to the provider rather than made a default header on the shared
    /// client because `Referer` means something else everywhere else: RFC 9110
    /// defines it as the page that linked to the request, and hosts act on it —
    /// origin checks, referer-gated access, analytics. Sending it to an MCP
    /// server or to Anthropic would assert a referring page that does not exist.
    pub(super) fn attribution(&self) -> &'static [(&'static str, &'static str)] {
        match self {
            Self::Anthropic | Self::OpenAI => &[],
            // Codex CLI names itself with this header; the backend may key on it.
            Self::OpenAICodex => &[("originator", "efference")],
            Self::OpenRouter => &[
                // The app's identity in OpenRouter's rankings. Required: without
                // it the other two headers do nothing.
                ("HTTP-Referer", "https://efference.net"),
                ("X-OpenRouter-Title", "efference"),
                // Marketplace placement, at most two per request. `cli-agent` is
                // the TUI and CLI verbs; `programming-app` is the web client.
                ("X-OpenRouter-Categories", "cli-agent,programming-app"),
            ],
        }
    }

    /// Whether this provider reads the OpenAI-style `prompt_cache_key` **body**
    /// field (docs/transport.md). OpenRouter does — it is the last fallback in
    /// its sticky-routing precedence, behind body `session_id` and the
    /// `x-session-id` header, and upstreams that speak the OpenAI dialect key
    /// their own prompt cache on it. Anthropic's Messages API has no such field,
    /// and an unrecognized top-level key is a 400 there, so gating is not
    /// optional here the way it is for a header.
    pub(super) fn prompt_cache_key_field(&self) -> bool {
        match self {
            Self::Anthropic => false,
            Self::OpenRouter | Self::OpenAI | Self::OpenAICodex => true,
        }
    }

    /// The literal prepended to the secret to form the auth header value.
    /// Split off the secret so the secret stays a *whole* value (an `op://…`
    /// reference `op run` can substitute — docs/transport.md).
    pub(super) fn auth_prefix(&self) -> &'static str {
        match self {
            Self::Anthropic => "",
            Self::OpenRouter | Self::OpenAI | Self::OpenAICodex => "Bearer ",
        }
    }

    /// The ceilings half of the boot metadata for `model` (docs/transport.md):
    /// per-upstream context and output caps, modalities, caching signals.
    /// OpenRouter model ids are `author/slug`, which its metadata routes take
    /// as two path segments; a slash-less id can't be queried and errors here,
    /// before any request. Effort levels are *not* on this route for
    /// OpenRouter — they come from [`Provider::effort_levels_url`].
    pub(super) fn capabilities_url(&self, model: &str) -> Result<String, Error> {
        match self {
            Self::Anthropic => Ok(format!("https://api.anthropic.com/v1/models/{model}")),
            Self::OpenRouter => {
                let (author, slug) = openrouter_slug(model)?;
                Ok(format!(
                    "https://openrouter.ai/api/v1/models/{author}/{slug}/endpoints"
                ))
            }
            Self::OpenAI | Self::OpenAICodex => Err("OpenAI capabilities come from the \
                                                    compiled catalog, not a metadata URL"
                .into()),
        }
    }

    /// The URL whose response names the model's effort levels, where a
    /// provider has one: OpenRouter's singular model entry
    /// (`GET /api/v1/model/{author}/{slug}`), whose `reasoning` block carries
    /// `supported_efforts` and `default_effort` (docs/transport.md).
    /// Anthropic's single metadata response names no efforts and there is no
    /// second route to ask — `None`, which leaves
    /// `ModelCapabilities::supported_efforts` unset and a configured level
    /// passes through to the provider's own validation.
    pub(super) fn effort_levels_url(&self, model: &str) -> Result<Option<String>, Error> {
        match self {
            Self::Anthropic | Self::OpenAI | Self::OpenAICodex => Ok(None),
            Self::OpenRouter => {
                let (author, slug) = openrouter_slug(model)?;
                Ok(Some(format!(
                    "https://openrouter.ai/api/v1/model/{author}/{slug}"
                )))
            }
        }
    }

    /// Parse this provider's metadata response into [`ModelCapabilities`]. A
    /// response missing the fields we need is an error (the boot step is
    /// required — docs/transport.md), never a silent default.
    ///
    /// `upstream` is the session's `--upstream` order (empty when unpinned). It
    /// narrows the endpoint set the OpenRouter aggregations run over: a pinned
    /// session routes to exactly those endpoints, so aggregating across the ones
    /// it will never reach reports someone else's ceiling. Anthropic ignores it —
    /// there is one upstream.
    pub(super) fn parse_capabilities(
        &self,
        body: &str,
        upstream: &[String],
    ) -> Result<ModelCapabilities, Error> {
        match self {
            Self::OpenAI | Self::OpenAICodex => {
                Err("OpenAI capabilities come from the compiled catalog".into())
            }
            Self::Anthropic => {
                // `GET /v1/models/{id}`: `max_input_tokens` is the context
                // window, `max_tokens` the output cap. Unknown fields are
                // ignored until a consumer needs them;
                // `capabilities.image_input.supported` is the one that has one.
                #[derive(Deserialize)]
                struct Model {
                    max_input_tokens: Option<u64>,
                    max_tokens: Option<u64>,
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
                let model: Model = serde_json::from_str(body)?;
                let context_length = model.max_input_tokens.ok_or_else(|| -> Error {
                    "Anthropic model metadata carried no max_input_tokens".into()
                })?;
                Ok(ModelCapabilities {
                    context_length,
                    max_output_tokens: model.max_tokens,
                    // Claude thinking is governed by the tier allowlist
                    // (`api::thinking`), not this bit — `supports_reasoning`
                    // describes OpenRouter's normalized `reasoning` parameter.
                    supports_reasoning: false,
                    // Absent defaults to **true**, unlike the ceilings above,
                    // which error on absence. The ceilings have no safe default
                    // (a guessed number is silently wrong); this one does — the
                    // Messages API has accepted image blocks on every Claude
                    // model since it shipped, so a metadata response that stops
                    // carrying the field must not disable a working tool.
                    accepts_temperature: true,
                    supports_image_input: model
                        .capabilities
                        .and_then(|c| c.image_input)
                        .and_then(|i| i.supported)
                        .unwrap_or(true),
                    // Anthropic's `cache_control` markers let the harness choose
                    // the write frontier (docs/transport.md).
                    prompt_cache: PromptCachePolicy::Explicit,
                    // No effort levels anywhere in Anthropic's metadata — not
                    // on this route and no second route to ask
                    // (`effort_levels_url`). A configured level passes through
                    // unvalidated; the provider's own 400 is the feedback.
                    supported_efforts: None,
                    default_effort: None,
                })
            }
            Self::OpenRouter => {
                // `GET /api/v1/models/{author}/{slug}/endpoints`: one entry per
                // upstream currently serving the model, each with its own
                // `context_length` / `max_completion_tokens`. The two are
                // aggregated in opposite directions because they are used in
                // opposite ways — see the field docs on `ModelCapabilities`.
                // `architecture.input_modalities` is model-level (one modality
                // set per slug, not per upstream); per-endpoint pricing is read
                // for the caching signal below.
                #[derive(Deserialize)]
                struct Body {
                    data: Data,
                }
                #[derive(Deserialize)]
                struct Data {
                    endpoints: Vec<ModelEndpoint>,
                    #[serde(default)]
                    architecture: Architecture,
                }
                #[derive(Deserialize, Default)]
                struct Architecture {
                    #[serde(default)]
                    input_modalities: Vec<String>,
                }
                #[derive(Deserialize)]
                struct ModelEndpoint {
                    context_length: Option<u64>,
                    max_completion_tokens: Option<u64>,
                    #[serde(default)]
                    supported_parameters: Vec<String>,
                    /// The endpoint slug `--upstream` names (`google-ai-studio`,
                    /// `google-vertex/global`) — OpenRouter calls it `tag` here
                    /// and reads it back as `provider.order` on a request.
                    #[serde(default)]
                    tag: Option<String>,
                    /// Whether this upstream caches prompts on its own, with no
                    /// marker in the request (Gemini, DeepSeek).
                    #[serde(default)]
                    supports_implicit_caching: bool,
                    #[serde(default)]
                    pricing: Pricing,
                }
                #[derive(Deserialize, Default)]
                struct Pricing {
                    /// Present (as a display string like `"$0.1/M tokens"`) when
                    /// the upstream prices cache reads separately — i.e. when it
                    /// has an explicit prompt cache at all.
                    #[serde(default)]
                    input_cache_read: Option<String>,
                }
                let body: Body = serde_json::from_str(body)?;
                let data = body.data;
                // The `--upstream` pin narrows the set before anything
                // aggregates over it (docs/transport.md). A pin naming no
                // endpoint in the listing leaves the set alone rather than
                // emptying it: the request would fail loudly on its own, and an
                // empty set here would fail boot with a message about
                // `context_length` that names the wrong problem.
                let pinned: Vec<&ModelEndpoint> = data
                    .endpoints
                    .iter()
                    .filter(|e| {
                        e.tag
                            .as_deref()
                            .is_some_and(|tag| upstream.iter().any(|slug| slug == tag))
                    })
                    .collect();
                let endpoints: Vec<&ModelEndpoint> = if pinned.is_empty() {
                    data.endpoints.iter().collect()
                } else {
                    pinned
                };
                // Descriptive ceiling: **max**. OpenRouter routes only to
                // endpoints that fit the request, so a prompt larger than the
                // smallest endpoint is not an error — it just narrows the
                // routable set. A min here under-reports by the width of the
                // narrowest outlier, which for a 1M-token model with one 256k
                // endpoint means reporting 256k (docs/transport.md).
                let context_length = endpoints
                    .iter()
                    .filter_map(|e| e.context_length)
                    .max()
                    .ok_or_else(|| -> Error {
                        "OpenRouter model metadata listed no endpoint with a context_length \
                         (is the model currently served?)"
                            .into()
                    })?;
                // Sent, not merely displayed: this is the cap each request's
                // `max_tokens` is shrunk from (`api/reservation.rs`), and
                // OpenRouter routes only to endpoints that support a response
                // that long. So **min** — a max would pin
                // routing to the single widest endpoint, changing price and
                // availability as a side effect of a display value — but the
                // min over endpoints that clear [`OUTPUT_FLOOR`]: an outlier
                // below it would otherwise cap every turn of the session at
                // its number (observed 2026-09-02: one of twenty-two
                // endpoints at 2048 held a thinking model to 2048 output
                // tokens per turn, docs/transport.md). Only when no endpoint
                // clears the floor is the plain min kept.
                let max_output_tokens =
                    output_ceiling(endpoints.iter().filter_map(|e| e.max_completion_tokens));
                // Conservative like `max_output_tokens`, and for the same
                // reason: routing is unpinned and a `thinking` block is not a
                // routing filter, so it is sent only if *every* endpoint
                // currently serving the model lists `reasoning`.
                let supports_reasoning = endpoints
                    .iter()
                    .all(|e| e.supported_parameters.iter().any(|p| p == "reasoning"));
                // Does a prompt cache exist behind this slug at all — the
                // question the keepalive is gated on (docs/transport.md). Two
                // signals, either sufficient: an upstream that caches on its own
                // (`supports_implicit_caching`), or one that prices cache reads
                // separately (`pricing.input_cache_read`), which is how the
                // explicit-`cache_control` upstreams — every Anthropic-compatible
                // one — report having a cache. **Any**, not all: sticky routing
                // pins a session to one upstream, so the question is whether a
                // reachable endpoint caches, and the runtime tripwire
                // (`daemon/keepalive.rs`) is what stops a ping that turns out to
                // cost something.
                let has_implicit_cache = endpoints.iter().any(|e| e.supports_implicit_caching);
                let has_explicit_cache = endpoints
                    .iter()
                    .any(|e| e.pricing.input_cache_read.is_some());
                let prompt_cache =
                    PromptCachePolicy::from_support(has_explicit_cache, has_implicit_cache);
                Ok(ModelCapabilities {
                    context_length,
                    max_output_tokens,
                    supports_reasoning,
                    // Model-level, not per-endpoint: one slug has one modality
                    // set whichever upstream serves it. An empty list (a
                    // response with no `architecture`) reads as text-only, which
                    // is the fail-closed direction for a field whose whole job is
                    // to stop bytes a model cannot read.
                    accepts_temperature: true,
                    supports_image_input: data
                        .architecture
                        .input_modalities
                        .iter()
                        .any(|m| m == "image"),
                    prompt_cache,
                    // The endpoints listing carries no effort levels — those
                    // come from the model entry, which `fetch_capabilities`
                    // fetches separately and merges in. Left `None` here so
                    // this parse stays an honest statement of what *this*
                    // response said.
                    supported_efforts: None,
                    default_effort: None,
                })
            }
        }
    }
    /// Parse the model-entry response into the effort levels it declares
    /// (docs/transport.md): `reasoning.supported_efforts` and
    /// `reasoning.default_effort`. An absent block, an absent field, or an
    /// empty list all read as `None` — the metadata declares no levels, and
    /// [`EffortLevels`]'s boot rule refuses a configured effort against that.
    /// Only the OpenRouter fetch reaches this: Anthropic names no efforts and
    /// has no model-entry route (`effort_levels_url` returns `None` there).
    /// The block's `mandatory` and `default_enabled` fields are observed in
    /// live metadata but unused — no consumer yet.
    pub(super) fn parse_model_entry(&self, body: &str) -> Result<EffortLevels, Error> {
        match self {
            Self::Anthropic => Err("Anthropic metadata names no effort levels".into()),
            Self::OpenAI | Self::OpenAICodex => {
                Err("OpenAI effort levels come from the compiled catalog".into())
            }
            Self::OpenRouter => {
                // Same envelope as every other OpenRouter response: the entry
                // sits under `data` (parse_capabilities parses the same
                // wrapper). A response without it is not the shape asked for
                // and errors — the fetch is required at boot.
                #[derive(Deserialize)]
                struct Response {
                    data: ModelEntry,
                }
                #[derive(Deserialize, Default)]
                struct ModelEntry {
                    #[serde(default)]
                    reasoning: Option<Reasoning>,
                }
                #[derive(Deserialize, Default)]
                struct Reasoning {
                    #[serde(default)]
                    supported_efforts: Option<Vec<String>>,
                    #[serde(default)]
                    default_effort: Option<String>,
                }
                let response: Response = serde_json::from_str(body)?;
                let reasoning = response.data.reasoning.unwrap_or_default();
                Ok(EffortLevels {
                    // An empty list is the same "declares nothing" state as an
                    // absent one: neither affirms a level.
                    supported: reasoning
                        .supported_efforts
                        .filter(|levels| !levels.is_empty()),
                    default: reasoning.default_effort,
                })
            }
        }
    }
}

/// OpenRouter model ids are `author/slug`, which its metadata routes take as
/// two path segments; a slash-less id can't be queried and errors here,
/// before any request.
fn openrouter_slug(model: &str) -> Result<(&str, &str), Error> {
    model.split_once('/').ok_or_else(|| -> Error {
        format!(
            "OpenRouter model ids are `author/slug` (e.g. \
             `anthropic/claude-haiku-4.5`); {model:?} has no author segment"
        )
        .into()
    })
}

/// The effort levels a model's metadata declares (docs/transport.md), parsed
/// from the OpenRouter model entry's `reasoning` block. `supported: None` —
/// block absent, field absent, or an empty list — means the metadata declares
/// no levels: a configured effort cannot be affirmatively confirmed against
/// it and is refused at boot. `default` is boot-line observability only; it
/// is never sent.
pub(super) struct EffortLevels {
    pub supported: Option<Vec<String>>,
    pub default: Option<String>,
}

/// The smallest per-turn output budget worth routing to (docs/transport.md).
/// OpenRouter's `max_completion_tokens` aggregates by **min** across the
/// endpoints serving a model, and the min is sent as `max_tokens`; an endpoint
/// far below its peers therefore caps the whole session at its number, and on
/// a model whose thinking counts against the cap that is a turn cut off
/// mid-deliberation. Endpoints below this line are left out of the min — the
/// request excludes them from routing, which is the intended effect.
pub const OUTPUT_FLOOR: u64 = 32_768;

/// Fold per-endpoint output caps into the one that is sent: the min over caps
/// at or above [`OUTPUT_FLOOR`], else the plain min (a model none of whose
/// endpoints clears the floor is a short-output model, and narrowing its
/// routing would buy nothing). `None` when no endpoint reports a cap.
pub(super) fn output_ceiling(caps: impl Iterator<Item = u64>) -> Option<u64> {
    let caps: Vec<u64> = caps.collect();
    caps.iter()
        .copied()
        .filter(|&c| c >= OUTPUT_FLOOR)
        .min()
        .or_else(|| caps.iter().copied().min())
}

/// Who chooses prompt-cache write boundaries (docs/transport.md).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PromptCachePolicy {
    /// No prompt cache is known to exist behind the selected model.
    None,
    /// The request dialect lets the harness choose the cache-write frontier.
    Explicit,
    /// The provider chooses cache-write locations from the request prefix.
    Implicit,
    /// Reachable upstreams expose both policies and routing does not reveal
    /// which one the sticky session selected.
    Mixed,
}

impl PromptCachePolicy {
    fn from_support(explicit: bool, implicit: bool) -> Self {
        match (explicit, implicit) {
            (false, false) => Self::None,
            (true, false) => Self::Explicit,
            (false, true) => Self::Implicit,
            (true, true) => Self::Mixed,
        }
    }

    /// Whether any prompt cache is known to exist behind the selected model.
    pub fn is_supported(self) -> bool {
        self != Self::None
    }
}

/// What the provider's capability source reports about the selected model.
/// Resolved once at boot — a **required** step; `serve` aborts if the lookup
/// fails (docs/transport.md) — held in memory, transport-only: never logged or
/// projected. Anthropic and OpenRouter supply remote metadata;
/// OpenAI's compiled catalog is in `provider/openai.rs` because its Models
/// endpoint does not report the limits and feature flags consumed here.
///
/// Grows one field per capability consumer: a new conditional (image input,
/// cache policy, pricing) is a field here plus its extraction from each
/// provider's source.
#[derive(Debug, Clone, Serialize)]
pub struct ModelCapabilities {
    /// Maximum prompt tokens the model accepts. Anthropic `max_input_tokens`;
    /// OpenRouter: the **max** `context_length` across the endpoints currently
    /// serving the model — routing filters to endpoints that fit the request,
    /// so a wide prompt narrows the routable set rather than failing
    /// (docs/transport.md).
    pub context_length: u64,
    /// Maximum response tokens, where the provider reports one. OpenRouter:
    /// the **min** `max_completion_tokens` across serving endpoints — unlike
    /// `context_length` this value reaches the wire — as the cap each request's
    /// `max_tokens` is shrunk from (`api/reservation.rs`) — and routing skips
    /// endpoints that cannot produce a response that long (docs/transport.md).
    pub max_output_tokens: Option<u64>,
    /// Whether every endpoint currently serving the model lists OpenRouter's
    /// normalized `reasoning` in `supported_parameters` — the foreign-model input
    /// to the thinking matrix (`api::thinking`; docs/transport.md). Always `false`
    /// from the Anthropic provider, whose thinking is governed by the Claude tier
    /// allowlist instead.
    pub supports_reasoning: bool,
    /// Whether the model accepts image blocks. Anthropic:
    /// `capabilities.image_input.supported`, defaulting to `true` when the field
    /// is absent (every Claude model on Messages takes images). OpenRouter:
    /// `architecture.input_modalities` contains `image`, defaulting to `false`
    /// when the listing carries no modalities.
    ///
    /// The consumer is the tool layer (docs/tools.md): `read_image` refuses
    /// before it decodes anything, and an MCP result's image blocks are replaced
    /// by a placeholder naming why. Sending image bytes to a model that reports
    /// no image input spends the upload on a 400 or on content the model cannot
    /// read, and the tool's own success message says it worked.
    pub supports_image_input: bool,
    /// Whether the model takes a sampling `temperature` (docs/transport.md).
    /// The compiled OpenAI catalog names the models that reject the parameter
    /// outright (GPT-6 Astra); every other source reports `true` and leaves an
    /// unsupported value to the provider's own validation. The consumer is the
    /// boot check: an explicit `--temperature` against a model that refuses one
    /// fails before any request is sent.
    pub accepts_temperature: bool,
    /// Anthropic is explicit; OpenRouter can expose explicit, implicit, mixed,
    /// or absent caching across the endpoints serving a slug. The
    /// keepalive checks [`PromptCachePolicy::is_supported`], but support alone
    /// does not imply that a dialect has a cheap refresh primitive.
    pub prompt_cache: PromptCachePolicy,
    /// The effort levels the model's metadata declares (docs/transport.md).
    /// OpenRouter: the model entry's `reasoning.supported_efforts`, with an
    /// absent or empty list normalized to `None` (`EffortLevels`). The
    /// consumer is the boot check: a configured `--effort`/`[transport]
    /// effort` is sent only when this list names it, because OpenRouter maps
    /// an unsupported level to the nearest one silently — a session could
    /// otherwise believe it set a knob that did nothing. `None` refuses every
    /// configured level. Anthropic: `None` — its metadata names no efforts,
    /// and a configured level passes through to the provider's own validation.
    pub supported_efforts: Option<Vec<String>>,
    /// The effort level the model's metadata says it defaults to — OpenRouter
    /// `reasoning.default_effort` — printed on the boot line even when no
    /// effort is configured, which answers "what will the knob actually do"
    /// in one glance (docs/transport.md). Never sent. Anthropic: `None`.
    pub default_effort: Option<String>,
}

impl ModelCapabilities {
    /// The boot check for a configured effort level (docs/transport.md),
    /// called for providers that validate (`Provider::validates_effort_levels`
    /// — OpenRouter). A model whose entry lists levels accepts only those:
    /// OpenRouter maps an unsupported level to the nearest supported one
    /// silently, so a session could otherwise believe it set a knob that did
    /// nothing. A model that declares no levels — absent block, absent field,
    /// or an empty list — refuses every configured level: nothing was
    /// affirmed, so nothing is sent. The membership test is exact string
    /// equality against the metadata's own spellings; the declared default,
    /// when present, is named as the unset path in the message.
    pub fn check_effort(&self, effort: &str, model: &str) -> Result<(), String> {
        match &self.supported_efforts {
            Some(levels) if levels.iter().any(|level| level == effort) => Ok(()),
            Some(levels) => Err(format!(
                "effort `{effort}` is not among the levels {model} reports ({}) — \
                 OpenRouter maps unsupported levels to the nearest silently; pick one of \
                 those or leave effort unset{}",
                levels.join(", "),
                self.default_effort
                    .as_deref()
                    .map(|default| format!(" (provider default: {default})"))
                    .unwrap_or_default()
            )),
            None => Err(format!(
                "{model}'s metadata declares no supported effort levels — a configured \
                 level cannot be affirmatively confirmed against it. Leave effort unset."
            )),
        }
    }
}

impl Endpoint {
    /// The required boot-time metadata lookup (docs/transport.md): GET the
    /// provider's model-metadata route(s) for `model` and parse them into
    /// [`ModelCapabilities`]. Authenticated with the same header as the
    /// messages POSTs (Anthropic's Models API requires the key; OpenRouter
    /// tolerates it). Any failure — network, non-2xx, missing fields — is an
    /// error the caller treats as fatal to startup.
    ///
    /// Anthropic asks one route ([`Provider::capabilities_url`]). OpenRouter
    /// asks two: the endpoints listing for the ceilings, then the singular
    /// model entry ([`Provider::effort_levels_url`]) for the effort levels,
    /// merged in here. Both are fatal at boot — the boot rule validates a
    /// configured effort against this metadata, so "the entry route didn't
    /// answer" must not silently degrade into no validation.
    ///
    /// `upstream` is the session's `--upstream` order (empty when unpinned): a
    /// pinned session's ceilings come from the endpoints it will actually reach,
    /// not from every endpoint serving the slug (docs/transport.md).
    pub async fn fetch_capabilities(
        &self,
        model: &str,
        upstream: &[String],
    ) -> Result<ModelCapabilities, Error> {
        if self.provider.uses_openai_catalog() {
            let mut capabilities = openai_model_capabilities(model)?;
            // The Codex backend rejects `prompt_cache_breakpoint` ("not supported on
            // this model", observed 2026-09-06 on gpt-5.6-luna), so on that arm the
            // provider places the breakpoints and the encoder emits no markers.
            if self.provider == Provider::OpenAICodex {
                capabilities.prompt_cache = PromptCachePolicy::Implicit;
            }
            return Ok(capabilities);
        }
        let url = self.provider.capabilities_url(model)?;
        let body = self.authed_get(&url).await?;
        let mut capabilities =
            self.provider
                .parse_capabilities(&body, upstream)
                .map_err(|e| -> Error {
                    format!("could not parse model metadata from {url}: {e}").into()
                })?;
        if let Some(effort_url) = self.provider.effort_levels_url(model)? {
            let effort_body = self.authed_get(&effort_url).await?;
            let efforts = self
                .provider
                .parse_model_entry(&effort_body)
                .map_err(|e| -> Error {
                    format!("could not parse model metadata from {effort_url}: {e}").into()
                })?;
            capabilities.supported_efforts = efforts.supported;
            capabilities.default_effort = efforts.default;
        }
        Ok(capabilities)
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
            Provider::OpenAI | Provider::OpenAICodex => {
                return Err("openai exposes no per-generation cost endpoint".into());
            }
        };
        parse_generation_cost(&self.authed_get(&url).await?)
    }

    /// One authenticated GET against a provider metadata/accounting URL,
    /// returning the body on 2xx and a status+body error otherwise. Draws the
    /// process-wide client (`http.rs`) rather than dialing a fresh connection per
    /// call — `fetch_generation_cost` alone can run thousands of these in a
    /// spotter-eval cost pass, and a fresh `Client::new` per call holds a socket
    /// per lookup for the life of the process. Sharing it with the agent's
    /// `Client` also means these lookups multiplex onto the HTTP/2 connection the
    /// turn stream already holds open to the same host (docs/transport.md).
    async fn authed_get(&self, url: &str) -> Result<String, Error> {
        let http = crate::http::client();
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

/// Whether an OpenRouter slug names a Gemini model — `google/gemini-3.7-flash`
/// and the like. Matched on the author prefix plus the family, so a
/// `google/`-authored non-Gemini slug is not swept in
/// (docs/transport.md).
fn is_gemini(model: &str) -> bool {
    let model = model.trim().to_ascii_lowercase();
    let slug = model.strip_prefix("google/").unwrap_or(&model);
    slug.starts_with("gemini")
}
