//! The HTTP shape of each provider's requests: its URLs, auth header, and
//! per-request headers (docs/transport.md).

use super::*;

impl Provider {
    /// The endpoint this provider serves *turns* on, or `None` for one that
    /// serves no inference ([`Self::inference_dialect`]). A decision-model
    /// provider has a route ([`Self::decisions_url`]) but not this one, and the
    /// two are not interchangeable — posting a turn body to the decisions route
    /// is a 422, not a degraded answer.
    ///
    /// `base` is the configured base URL, present only for [`Self::Custom`]; the
    /// named providers own their URLs and ignore it.
    pub(in crate::api) fn messages_url(&self, base: Option<&str>) -> Option<String> {
        match self {
            Self::Anthropic => Some("https://api.anthropic.com/v1/messages".to_string()),
            Self::OpenRouter => Some("https://openrouter.ai/api/v1/messages".to_string()),
            Self::OpenAI => Some("https://api.openai.com/v1/responses".to_string()),
            Self::OpenAICodex => {
                Some("https://chatgpt.com/backend-api/codex/responses".to_string())
            }
            Self::TypeSafe => None,
            // A System One endpoint has no turns route; its one route is
            // [`Self::decisions_url`].
            Self::Custom(WireDialect::SystemOne) => None,
            // The dialect picks the path: these two are the conventional routes,
            // and an endpoint that serves the dialect on a different path is a
            // base URL that includes it.
            Self::Custom(WireDialect::Messages) => Some(join(base?, "messages")),
            Self::Custom(WireDialect::Responses) => Some(join(base?, "responses")),
            Self::Custom(WireDialect::Completions) => Some(join(base?, "chat/completions")),
        }
    }

    /// The Decisions route (docs/permissions.md), for providers that serve a
    /// structured-decision model. `None` everywhere else: this is a distinct API
    /// — a single JSON POST of `{model, state, questions}` answered with a
    /// probability per question, not a Messages or Responses request — so a
    /// provider either serves it or the spotter band that uses it is unreachable
    /// there.
    pub(in crate::api) fn decisions_url(&self, base: Option<&str>) -> Option<String> {
        match self {
            Self::TypeSafe => Some("https://api.typesafe.ai/v1/systemone".to_string()),
            // OpenRouter fronts the same models on its own alpha route, which is
            // how this rung was reachable before first-party access existed.
            Self::OpenRouter => Some("https://openrouter.ai/api/alpha/decisions".to_string()),
            Self::Anthropic | Self::OpenAI | Self::OpenAICodex => None,
            // The configured URL is the route, whole. Providers that adopted the
            // contract spell its path differently (`/v1/systemone`,
            // `/decisions/v1/systemone`, `/v1/decisions`, a per-model run URL),
            // so appending a conventional segment would be wrong for most of them.
            Self::Custom(WireDialect::SystemOne) => base.map(str::to_string),
            // A turn dialect says nothing about serving the System One contract.
            Self::Custom(
                WireDialect::Messages | WireDialect::Responses | WireDialect::Completions,
            ) => None,
        }
    }

    /// The auth header **name** this provider reads.
    /// Whether this provider's decisions route accepts a `session_id` for
    /// grouping. OpenRouter documents it alongside `user` and `trace`; the
    /// first-party API documents `state`, `model`, and `questions` only, and
    /// validates the body, so the field is sent only where it is named.
    pub(in crate::api) fn decisions_takes_session_id(&self) -> bool {
        matches!(self, Self::OpenRouter)
    }

    pub(in crate::api) fn auth_header(&self) -> &'static str {
        match self {
            Self::Anthropic => "x-api-key",
            Self::OpenRouter
            | Self::OpenAI
            | Self::OpenAICodex
            | Self::TypeSafe
            // `x-api-key` is Anthropic's own spelling, not a dialect's: the
            // Anthropic-shaped endpoints seen elsewhere read `authorization`.
            | Self::Custom(_) => "authorization",
        }
    }

    /// The header this provider reads a sticky-routing session key from, or
    /// `None` where the concept doesn't exist (docs/transport.md). Anthropic has
    /// no equivalent: nothing in Messages selects an upstream, because there is
    /// only one — `metadata.user_id` is an abuse-detection identifier, not a
    /// routing key, and is not a substitute. Gating here rather than sending the
    /// header everywhere means the design never rests on an unrecognized header
    /// being ignored rather than rejected.
    ///
    /// The Codex backend takes the key on `session-id` and echoes it back as the
    /// response's `prompt_cache_key`; the **body** field of that name is discarded
    /// there without an error, so sending only the body copy leaves the backend
    /// minting a fresh per-request key — one of the miss reasons OpenAI's own
    /// diagnostics enumerate (`prompt_cache_key_changed`), and enough to cost a
    /// session most of its prefix cache. The spelling is Codex CLI's
    /// (`build_session_headers`); the backend answers to `session_id` too, but
    /// matching the client it is built for is the safer of the two. Its companion
    /// `thread-id` sets no key on its own and is not sent.
    pub(in crate::api) fn session_header(&self) -> Option<&'static str> {
        match self {
            // Sticky routing is a fact about a specific multi-upstream
            // deployment. An unrecognized header may be rejected rather than
            // ignored, so a custom endpoint is sent none.
            Self::Anthropic | Self::OpenAI | Self::TypeSafe | Self::Custom(_) => None,
            Self::OpenAICodex => Some("session-id"),
            Self::OpenRouter => Some("x-session-id"),
        }
    }

    /// The header a sticky-routing token for the user turn in flight travels on,
    /// in both directions (docs/transport.md): the backend sets it on a response,
    /// the client replays it on every later request of the same turn. Only the
    /// Codex backend has this contract; Codex CLI treats it as required. `None`
    /// means neither capture nor replay happens.
    pub(in crate::api) fn turn_routing_header(&self) -> Option<&'static str> {
        match self {
            Self::OpenAICodex => Some("x-codex-turn-state"),
            Self::Anthropic
            | Self::OpenAI
            | Self::OpenRouter
            | Self::TypeSafe
            | Self::Custom(_) => None,
        }
    }

    /// Whether responses carry the subscription-window headers `api/plan.rs`
    /// reads (docs/transport.md). Only the Codex backend does.
    pub(in crate::api) fn reports_plan_windows(&self) -> bool {
        matches!(self, Self::OpenAICodex)
    }

    /// The request header naming the model for the backend's router, sent as
    /// Codex CLI sends it (`model=<slug>`) on the Codex backend only.
    pub(in crate::api) fn routing_hint_header(&self) -> Option<&'static str> {
        match self {
            Self::OpenAICodex => Some("x-codex-routing-hint"),
            Self::Anthropic
            | Self::OpenAI
            | Self::OpenRouter
            | Self::TypeSafe
            | Self::Custom(_) => None,
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
    pub(in crate::api) fn attribution(&self) -> &'static [(&'static str, &'static str)] {
        match self {
            // Attribution is a per-marketplace concept, and `Referer` asserts a
            // referring page to an endpoint that may act on it (see above), so a
            // custom endpoint is told nothing about the caller.
            Self::Anthropic | Self::OpenAI | Self::TypeSafe | Self::Custom(_) => &[],
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
    ///
    /// True on the Codex arm records the documented contract rather than an
    /// effect: that backend accepts the field and discards it, keying on the
    /// `session-id` header instead ([`Self::session_header`], which is what
    /// actually carries the key there). Kept rather than switched off so the arm
    /// starts benefiting if the backend begins honouring the body field, since
    /// the value sent is the same one either way.
    pub(in crate::api) fn prompt_cache_key_field(&self) -> bool {
        match self {
            Self::Anthropic | Self::TypeSafe | Self::Custom(WireDialect::SystemOne) => false,
            Self::OpenRouter | Self::OpenAI | Self::OpenAICodex => true,
            // The field is an OpenAI-dialect one, and an unrecognized top-level key
            // is a 400 on Messages — the same reason this is gated for Anthropic.
            Self::Custom(WireDialect::Messages) => false,
            Self::Custom(WireDialect::Responses) => true,
            // Not every Completions server tolerates an unknown top-level key; one
            // that keys its cache on this field takes it through the connection's
            // static body.
            Self::Custom(WireDialect::Completions) => false,
        }
    }

    /// Whether this provider reads the Messages API's top-level `diagnostics` object
    /// (docs/transport.md) — the cache-diagnostics opt-in, whose
    /// `previous_message_id` names the response the provider should compare this
    /// request's prompt against.
    ///
    /// Native Anthropic only. The feature is Claude-API-only: Bedrock and Vertex do
    /// not implement it, and an unrecognized top-level key is a 400 on Messages, which
    /// is why this is a gate rather than a header-style nicety. OpenRouter's
    /// `/api/v1/messages` skin was measured accepting the object and answering
    /// `"diagnostics": null` while serving the request from AWS, so a session there
    /// would read a constant null it cannot tell apart from "no divergence" or from a
    /// quiet drop. The custom Messages endpoints inherit that ambiguity,
    /// and a fingerprint's scope is the caller's organization and workspace, which
    /// through a router is not ours.
    pub(in crate::api) fn cache_diagnostics_field(&self) -> bool {
        matches!(self, Self::Anthropic)
    }

    /// The literal prepended to the secret to form the auth header value.
    /// Split off the secret so the secret stays a *whole* value (an `op://…`
    /// reference `op run` can substitute — docs/transport.md).
    pub(in crate::api) fn auth_prefix(&self) -> &'static str {
        match self {
            Self::Anthropic => "",
            Self::OpenRouter
            | Self::OpenAI
            | Self::OpenAICodex
            | Self::TypeSafe
            | Self::Custom(_) => "Bearer ",
        }
    }

    /// The assembled auth header value for a bare secret. The one place the
    /// prefix is applied, so a credential re-read under a running host
    /// (`AuthValue`, docs/transport.md) reproduces exactly what construction
    /// built rather than a second spelling of it.
    pub fn auth_header_value(&self, secret: &str) -> String {
        format!("{}{}", self.auth_prefix(), secret)
    }

    /// The ceilings half of the boot metadata for `model` (docs/transport.md):
    /// per-upstream context and output caps, modalities, caching signals.
    /// OpenRouter model ids are `author/slug`, which its metadata routes take
    /// as two path segments; a slash-less id can't be queried and errors here,
    /// before any request. Effort levels are *not* on this route for
    /// OpenRouter — they come from [`Provider::effort_levels_url`].
    pub(in crate::api) fn capabilities_url(
        &self,
        base: Option<&str>,
        model: &str,
    ) -> Result<String, Error> {
        match self {
            Self::Anthropic => Ok(format!("https://api.anthropic.com/v1/models/{model}")),
            // The listing, not a per-model route: where a custom endpoint discloses
            // capabilities at all it does so there, and the same response answers
            // the credential check and the model menu (`provider/listing.rs`). The
            // model is found within it rather than addressed by URL.
            Self::Custom(WireDialect::SystemOne) => Err("a System One connection publishes no \
                                                       model metadata"
                .into()),
            Self::Custom(_) => Ok(join(
                base.ok_or_else(|| -> Error { "a custom connection has no base URL".into() })?,
                "models",
            )),
            Self::OpenRouter => {
                let (author, slug) = openrouter_slug(model)?;
                Ok(format!(
                    "https://openrouter.ai/api/v1/models/{author}/{slug}/endpoints"
                ))
            }
            Self::OpenAI | Self::OpenAICodex => Err("OpenAI capabilities come from the \
                                                    compiled catalog, not a metadata URL"
                .into()),
            // A decision model has no context window, output cap, or cache policy
            // to look up: the request is one state and a set of questions, and the
            // only per-model fact is whether the id exists. Boot checks that against
            // the model listing instead (`Endpoint::check_decision_model`).
            Self::TypeSafe => {
                Err("typesafe models report no inference capabilities to fetch".into())
            }
        }
    }

    /// The route that lists the models this provider serves (docs/transport.md):
    /// what a discovery menu offers, what `efference dev models` reads, and for
    /// the Codex arm the source its capabilities come from.
    ///
    /// The Codex arm is the subscription backend's own listing, not the
    /// platform's `/v1/models`: a ChatGPT grant authenticates against
    /// `chatgpt.com/backend-api`, and the catalog that route serves carries the
    /// per-model context window, reasoning levels, and input modalities the
    /// platform listing omits (Codex CLI reads the same route). That route
    /// requires a `client_version` and filters the catalog by it, so the claim
    /// is part of the route rather than a parameter of it
    /// ([`Self::models_url`], `api/provider/codex.rs`).
    pub fn models_url(&self, base: Option<&str>) -> Option<String> {
        Some(match self {
            Self::Anthropic => "https://api.anthropic.com/v1/models?limit=100".to_string(),
            Self::OpenRouter => "https://openrouter.ai/api/v1/models".to_string(),
            Self::OpenAI => "https://api.openai.com/v1/models".to_string(),
            Self::OpenAICodex => format!(
                "https://chatgpt.com/backend-api/codex/models?client_version={}",
                codex::CLAIMED_CLIENT_VERSION
            ),
            Self::TypeSafe => "https://api.typesafe.ai/v1/models".to_string(),
            // A described endpoint's listing is the route its credential check
            // and capability lookup already use, whatever dialect it speaks
            // (`Provider::capabilities_url`).
            Self::Custom(WireDialect::SystemOne) => return None,
            Self::Custom(_) => join(base?, "models"),
        })
    }

    /// A GET that answers 2xx only to a valid credential, for `efference setup`'s
    /// live check (docs/operating.md): Anthropic's and OpenAI's model listings
    /// both require the key, and OpenRouter's key-info route describes the key
    /// itself. The Codex backend has no cheap authenticated read and answers
    /// `None`.
    pub(in crate::api) fn key_check_url(&self, base: Option<&str>) -> Option<String> {
        match self {
            Self::Anthropic => Some("https://api.anthropic.com/v1/models?limit=1".to_string()),
            Self::OpenRouter => Some("https://openrouter.ai/api/v1/key".to_string()),
            Self::OpenAI => Some("https://api.openai.com/v1/models".to_string()),
            Self::TypeSafe => Some("https://api.typesafe.ai/v1/models".to_string()),
            Self::OpenAICodex => None,
            // Both dialects converge on this route, and an endpoint serving
            // neither a listing nor a key check is one whose credential is first
            // tested by a turn. A System One endpoint's URL is the one route it
            // has: it publishes no sibling listing to read a key check from.
            Self::Custom(WireDialect::SystemOne) => None,
            Self::Custom(
                WireDialect::Messages | WireDialect::Responses | WireDialect::Completions,
            ) => Some(join(base?, "models")),
        }
    }

    /// The URL of a second metadata response naming the model's effort levels,
    /// where a provider has one: OpenRouter's singular model entry
    /// (`GET /api/v1/model/{author}/{slug}`), whose `reasoning` block carries
    /// `supported_efforts` and `default_effort` (docs/transport.md). Anthropic
    /// names its levels in the capabilities response itself, so `None` there.
    pub(in crate::api) fn effort_levels_url(&self, model: &str) -> Result<Option<String>, Error> {
        match self {
            Self::Anthropic
            | Self::OpenAI
            | Self::OpenAICodex
            | Self::TypeSafe
            // A listing carries no effort levels, and there is no second route to
            // ask at an endpoint this crate did not choose.
            | Self::Custom(_) => Ok(None),
            Self::OpenRouter => {
                let (author, slug) = openrouter_slug(model)?;
                Ok(Some(format!(
                    "https://openrouter.ai/api/v1/model/{author}/{slug}"
                )))
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

/// A configured base URL joined to one of a dialect's conventional path
/// segments. The base's trailing slash is optional — both spellings of the same
/// endpoint are what an operator will paste — so exactly one separator is
/// produced either way.
fn join(base: &str, segment: &str) -> String {
    format!("{}/{segment}", base.trim_end_matches('/'))
}
