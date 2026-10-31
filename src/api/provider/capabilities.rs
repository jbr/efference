//! What the boot capability fetch learns about a model, and how each
//! provider's metadata response is parsed into it (docs/transport.md).

use super::*;

impl Provider {
    /// Parse this provider's metadata response into [`ModelCapabilities`]. A
    /// response missing the fields we need is an error (the boot step is
    /// required — docs/transport.md), never a silent default.
    ///
    /// `upstream` is the session's `--upstream` order (empty when unpinned). It
    /// narrows the endpoint set the OpenRouter aggregations run over: a pinned
    /// session routes to exactly those endpoints, so aggregating across the ones
    /// it will never reach reports someone else's ceiling. Anthropic ignores it —
    /// there is one upstream.
    pub(in crate::api) fn parse_capabilities(
        &self,
        body: &str,
        model: &str,
        upstream: &[String],
    ) -> Result<ModelCapabilities, Error> {
        match self {
            // Opportunistic and field-by-field, because the listing's shape is
            // whatever the endpoint returns (`provider/listing.rs`).
            Self::Custom(_) => listing::capabilities_for(body, model),
            Self::OpenAI | Self::OpenAICodex => {
                Err("OpenAI capabilities come from the compiled catalog".into())
            }
            Self::TypeSafe => Err("typesafe publishes no inference capabilities".into()),
            Self::Anthropic => anthropic::capabilities(body),
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
                    /// Decimal USD per token, like `input_cache_read` where that is
                    /// a price at all; they feed the keepalive's affordability
                    /// check (`prices::conservative`).
                    #[serde(default)]
                    prompt: Option<String>,
                    #[serde(default)]
                    completion: Option<String>,
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
                // its number (one of twenty-two
                // endpoints at 2048 holds a thinking model to 2048 output
                // tokens per turn, docs/transport.md). Only when no endpoint
                // clears the floor is the plain min kept.
                let max_output_tokens =
                    output_ceiling(endpoints.iter().filter_map(|e| e.max_completion_tokens));
                // Conservative like `max_output_tokens`, and for the same
                // reason: routing is unpinned and a `thinking` block is not a
                // routing filter, so it is sent only if *every* endpoint
                // currently serving the model lists `reasoning`.
                let adaptive = endpoints
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
                // The same pinned set, so a pinned session is priced by the
                // upstreams it can reach.
                let prices = prices::conservative(endpoints.iter().map(|e| prices::ListedPrices {
                    prompt: e.pricing.prompt.as_deref(),
                    completion: e.pricing.completion.as_deref(),
                    cache_read: e.pricing.input_cache_read.as_deref(),
                }));
                Ok(ModelCapabilities {
                    context_length: Some(context_length),
                    max_output_tokens,
                    // How to say thinking off comes from the model entry's
                    // `reasoning.mandatory`, merged in by `fetch_capabilities`;
                    // until then, omission, which asserts nothing.
                    thinking: ThinkingSupport {
                        adaptive,
                        off: ThinkingOff::Omit,
                    },
                    // Model-level, not per-endpoint: one slug has one modality
                    // set whichever upstream serves it. An empty list (a
                    // response with no `architecture`) declares nothing, and
                    // unknown reads as capable: a refusal is recovered from,
                    // a gate left closed gives no signal (docs/transport.md).
                    accepts_temperature: true,
                    supports_image_input: {
                        let modalities = &data.architecture.input_modalities;
                        modalities.is_empty() || modalities.iter().any(|m| m == "image")
                    },
                    prompt_cache,
                    // The endpoints listing carries no effort levels — those
                    // come from the model entry, which `fetch_capabilities`
                    // fetches separately and merges in. Left `None` here so
                    // this parse stays an honest statement of what *this*
                    // response said; the flag follows the merged levels, which
                    // is where a source can affirm them.
                    supported_efforts: None,
                    default_effort: None,
                    validates_efforts: false,
                    prices,
                })
            }
        }
    }
    /// Parse the model-entry response into the effort levels it declares
    /// (docs/transport.md): `reasoning.supported_efforts` and
    /// `reasoning.default_effort`. An absent block, an absent field, or an
    /// empty list all read as `None` — the metadata declares no levels, and
    /// [`EffortLevels`]'s boot rule refuses a configured effort against that.
    /// Only the OpenRouter fetch reaches this: Anthropic names its levels in the
    /// capabilities response (`effort_levels_url` returns `None` there).
    /// `mandatory` decides how thinking off is spelled ([`ThinkingOff`]);
    /// `default_enabled` has no consumer.
    pub(in crate::api) fn parse_model_entry(&self, body: &str) -> Result<EffortLevels, Error> {
        match self {
            // Unreachable: `effort_levels_url` answers `None`, so nothing fetches a
            // model entry to parse. The arm exists because the match is exhaustive.
            Self::Custom(_) => Err("a custom endpoint publishes no model entry".into()),
            Self::Anthropic => {
                Err("Anthropic effort levels come from the capabilities response".into())
            }
            Self::OpenAI | Self::OpenAICodex => {
                Err("OpenAI effort levels come from the model catalog".into())
            }
            Self::TypeSafe => Err("a decision model takes no effort level".into()),
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
                    #[serde(default)]
                    mandatory: Option<bool>,
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
                    mandatory: reasoning.mandatory.unwrap_or(false),
                })
            }
        }
    }
}

/// The effort levels a model's metadata declares (docs/transport.md), parsed
/// from the OpenRouter model entry's `reasoning` block. `supported: None` —
/// block absent, field absent, or an empty list — means the metadata declares
/// no levels: a configured effort cannot be affirmatively confirmed against
/// it and is refused at boot. `default` is boot-line observability only; it
/// is never sent.
pub(in crate::api) struct EffortLevels {
    pub supported: Option<Vec<String>>,
    pub default: Option<String>,
    /// The entry's `reasoning.mandatory`: the model cannot be asked not to
    /// reason, so an explicit `thinking: disabled` is refused upstream
    /// (`api::thinking`). Absent reads as optional.
    pub mandatory: bool,
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
fn output_ceiling(caps: impl Iterator<Item = u64>) -> Option<u64> {
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
    /// Maximum prompt tokens the model accepts, or `None` where the source
    /// declares none — a custom endpoint whose listing carries no window
    /// (`provider/listing.rs`). Unknown is not a number to guess: the consumers
    /// already tolerate its absence, since `Client` and `api/reservation.rs` take
    /// an `Option` and `GET /state` reports a null context. What is lost is the
    /// occupancy meter and the pre-emptive fit; what remains is the provider's own
    /// refusal, which the halving retry answers.
    ///
    /// Anthropic `max_input_tokens`;
    /// OpenRouter: the **max** `context_length` across the endpoints currently
    /// serving the model — routing filters to endpoints that fit the request,
    /// so a wide prompt narrows the routable set rather than failing
    /// (docs/transport.md).
    pub context_length: Option<u64>,
    /// Maximum response tokens, where the provider reports one. OpenRouter:
    /// the **min** `max_completion_tokens` across serving endpoints — unlike
    /// `context_length` this value reaches the wire — as the cap each request's
    /// `max_tokens` is shrunk from (`api/reservation.rs`) — and routing skips
    /// endpoints that cannot produce a response that long (docs/transport.md).
    pub max_output_tokens: Option<u64>,
    /// How the model takes the `thinking` field: whether an `auto` turn may think
    /// adaptively, and how a request says thinking off (`api::thinking`,
    /// docs/transport.md). Anthropic: `capabilities.thinking.types`. OpenRouter:
    /// adaptive only when every endpoint currently serving the model lists
    /// `reasoning` in `supported_parameters` — conservative like
    /// `max_output_tokens`, since `thinking` is not a routing filter — and off
    /// spelled from the model entry's `reasoning.mandatory`. Codex: adaptive when
    /// the catalog entry names reasoning levels. Every other source:
    /// [`ThinkingSupport::UNKNOWN`].
    pub thinking: ThinkingSupport,
    /// Whether the model accepts image blocks: `false` only where a source
    /// declares it. Anthropic: `capabilities.image_input.supported`, defaulting to
    /// `true` when the field is absent (every Claude model on Messages takes
    /// images). OpenRouter: `architecture.input_modalities` contains `image`, or
    /// the list is empty. A described endpoint's listing: its modalities, `true`
    /// when it names none.
    ///
    /// Unknown reads as capable because the two errors differ in what they
    /// show (docs/transport.md): a model sent an image it cannot read answers
    /// with a 400 the client recovers from by withholding images, while a gate
    /// left closed on a capable model gives no signal at all.
    ///
    /// The consumer is the tool layer (docs/tools.md): where this is `false`,
    /// `read_image` refuses before it decodes anything, and an MCP result's image
    /// blocks are replaced by a placeholder naming why.
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
    /// absent or empty list normalized to `None` (`EffortLevels`). Anthropic:
    /// the levels `capabilities.effort` marks supported, an empty list when the
    /// model takes no effort at all. The consumer is the boot check: a
    /// configured `--effort`/`[transport] effort` is sent only when this list
    /// names it, where [`Self::validates_efforts`] says the list binds.
    pub supported_efforts: Option<Vec<String>>,
    /// The effort level the model's metadata says it defaults to — OpenRouter
    /// `reasoning.default_effort` — printed on the boot line even when no
    /// effort is configured, which answers "what will the knob actually do"
    /// in one glance (docs/transport.md). Never sent. Anthropic's metadata names
    /// no default: `None`.
    pub default_effort: Option<String>,
    /// Whether a configured effort level is checked against `supported_efforts`
    /// at all (docs/transport.md). True where the source declares the same
    /// values the request carries: OpenRouter's model entry, which matters
    /// because OpenRouter maps an unsupported level to the nearest supported one
    /// silently, and Anthropic's `capabilities.effort`, whose keys are
    /// `output_config.effort`'s own spellings.
    ///
    /// False where a source names levels that are not the request's values, or
    /// names none. A custom endpoint's listing declares none. A Codex catalog
    /// entry lists what Codex CLI offers — a set that includes a delegation mode
    /// the Responses API rejects and omits values it accepts — so its levels are
    /// reported and not enforced (`api/provider/codex.rs`). An unvalidated level
    /// passes through and the provider answers, naming every value it accepts.
    pub validates_efforts: bool,
    /// The prices the keepalive's affordability check weighs a ping's output
    /// against (docs/transport.md), when the source lists them for an endpoint
    /// that prices cache reads. OpenRouter only, aggregated conservatively over the
    /// endpoints the session can reach ([`TokenPrices`], `prices::conservative`);
    /// `None` elsewhere, where the check uses its token-ratio rule. Transport-only:
    /// never serialized, logged, or projected.
    #[serde(skip)]
    pub prices: Option<TokenPrices>,
}

impl ModelCapabilities {
    /// What a source that declares nothing reports: every capability at the arm
    /// that asserts least (docs/transport.md). The base a listing parse fills in
    /// field by field, and the whole answer for a model a listing does not name.
    ///
    /// Not a stand-in for a lookup that should have happened — a named provider's
    /// metadata fetch stays required, and failing it is still fatal at boot. This
    /// is the honest answer where there is no source to fail.
    pub fn unknown() -> Self {
        Self {
            context_length: None,
            max_output_tokens: None,
            thinking: ThinkingSupport::UNKNOWN,
            // Unknown reads as capable: see the field.
            supports_image_input: true,
            accepts_temperature: true,
            prompt_cache: PromptCachePolicy::None,
            supported_efforts: None,
            default_effort: None,
            // Nothing declared the levels, so nothing is checked: see the field.
            validates_efforts: false,
            prices: None,
        }
    }

    /// The boot check for a configured effort level (docs/transport.md), for a
    /// source that declared the levels ([`Self::validates_efforts`]). A model
    /// whose entry lists levels accepts only those: OpenRouter maps an
    /// unsupported level to the nearest supported one silently, so a session
    /// could otherwise believe it set a knob that did nothing. The membership
    /// test is exact string equality against the metadata's own spellings; the
    /// declared default, when present, is named as the unset path in the
    /// message.
    ///
    /// Without declared levels there is nothing to check against, and the level
    /// is left for the provider to accept or refuse. That is the honest reading
    /// of a source that names none, and on the OpenAI arms it is also what keeps
    /// a newly published model usable: its catalog entry may be missing while
    /// the model is not.
    pub fn check_effort(&self, effort: &str, model: &str) -> Result<(), String> {
        if !self.validates_efforts {
            return Ok(());
        }
        match &self.supported_efforts {
            Some(levels) if levels.iter().any(|level| level == effort) => Ok(()),
            Some(levels) if levels.is_empty() => Err(format!(
                "effort `{effort}`: {model}'s metadata says it takes no effort level. \
                 Leave effort unset."
            )),
            Some(levels) => Err(format!(
                "effort `{effort}` is not among the levels {model} reports ({}) — a \
                 provider may map an unsupported level to the nearest one silently; pick \
                 one of those or leave effort unset{}",
                levels.join(", "),
                self.default_effort
                    .as_deref()
                    .map_or_default(|default| format!(" (provider default: {default})"))
            )),
            None => Err(format!(
                "{model}'s metadata declares no supported effort levels — a configured \
                 level cannot be affirmatively confirmed against it. Leave effort unset."
            )),
        }
    }
}
