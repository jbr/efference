//! Provider identity and the model-metadata (capability) query it answers
//! (docs/transport.md).
//!
//! The provider is **declared** by a host connection's configuration, never
//! inferred from a URL shape (sniffing fails for proxies and custom URLs). It is a closed enum,
//! grown in code as providers are added — an externally-defined provider would
//! have to supply everything a variant supplies (endpoint, auth shape, and
//! capability source), and that interface is deferred until
//! the in-code providers show what it must carry.
//!
//! The enum itself is [`crate::record::log::Provider`], because a boot records it; this file holds
//! the identity's serde impls and the per-provider policy (cache lifetime, keepalive
//! cap, placements, default models). The HTTP shape of a provider's requests is
//! `provider/http.rs`; the capability types and their per-provider parses are
//! `provider/capabilities.rs`; the lookups an [`Endpoint`] makes are
//! `provider/fetch.rs`.

use super::*;

mod anthropic;
mod capabilities;
mod codex;
mod dialect;
mod fetch;
mod http;
mod listing;
mod models;
mod openrouter;
mod prices;
mod row;
mod typesafe;

pub use capabilities::{ModelCapabilities, OUTPUT_FLOOR, PromptCachePolicy};
#[cfg(test)]
pub(crate) use codex::seed_codex_catalog;
#[cfg(test)]
pub(in crate::api) use fetch::parse_generation_cost;
pub use prices::TokenPrices;
pub use row::{ListedModel, ModelKind, Price};

/// The spotter model a bare session uses on either OpenAI arm when the
/// connection names none of its own (docs/permissions.md). A single default
/// rather than a catalog: the spotter shares the endpoint and wants the cheap
/// band, and the arm's catalog reports no cost to pick from.
const OPENAI_DEFAULT_SPOTTER_MODEL: &str = "gpt-5.6-luna";

impl Serialize for Provider {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.name())
    }
}

/// The channel's spelling of a provider as a schema: one string per [`Provider::name`]
/// (docs/control-api.md). Built from the tables the parsers use, so a provider added to
/// either table reaches the schema; the hand-written [`Serialize`] is invisible to the derive.
#[cfg(test)]
impl schemars::JsonSchema for Provider {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "Provider".into()
    }

    fn json_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
        let mut names: Vec<&str> = Self::NAMED.iter().map(|(_, p, _)| p.name()).collect();
        names.dedup();
        names.extend(
            WireDialect::NAMED
                .iter()
                .map(|(_, dialect, _)| Self::Custom(*dialect).name()),
        );
        schemars::json_schema!({
            "description": "Which API family fronts a session's endpoint, spelled as its name.",
            "type": "string",
            "enum": names,
        })
    }
}

/// Read back what [`Serialize`] wrote, so a control client decodes the host's
/// own types instead of carrying a parallel copy of them. The accepted
/// vocabulary is [`Provider::name`]'s output, which is wider than
/// [`Provider::from_name`]'s: `custom-messages` and `custom-responses` are
/// display names the wire emits and configuration never spells, so the config
/// parser stays closed to them while the wire round-trips.
impl<'de> Deserialize<'de> for Provider {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let name = String::deserialize(deserializer)?;
        match name.as_str() {
            "custom-messages" => Ok(Self::Custom(WireDialect::Messages)),
            "custom-responses" => Ok(Self::Custom(WireDialect::Responses)),
            "custom-completions" => Ok(Self::Custom(WireDialect::Completions)),
            _ => Self::from_name(&name).map_err(serde::de::Error::custom),
        }
    }
}

/// Which Responses request shape a provider takes (docs/transport.md). The
/// platform endpoint documents the full parameter set; the Codex backend is
/// undocumented, so that arm sends only what Codex CLI itself sends.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResponsesProfile {
    Platform,
    Codex,
}

impl std::fmt::Display for Provider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.name())
    }
}

impl Provider {
    /// The wire dialect this provider speaks a *turn* in (docs/transport.md), or
    /// `None` for a provider that serves no inference at all.
    ///
    /// `None` is not a gap to fill in later: a decision-model provider has no
    /// assistant turn to encode or assemble, so there is no dialect it could
    /// honestly claim. [`Self::serves_inference`] is the same fact as a
    /// predicate, and resolution checks it, so every caller that unwraps this
    /// is downstream of a role that already refused the provider.
    pub fn inference_dialect(&self) -> Option<WireDialect> {
        match self {
            Self::Anthropic | Self::OpenRouter => Some(WireDialect::Messages),
            Self::OpenAI | Self::OpenAICodex => Some(WireDialect::Responses),
            Self::TypeSafe | Self::Custom(WireDialect::SystemOne) => None,
            Self::Custom(dialect) => Some(*dialect),
        }
    }

    /// Whether this provider can serve a session's main model or its triage
    /// triage. False only for a decision-model provider, which answers questions
    /// about a state and never produces a turn.
    pub fn serves_inference(&self) -> bool {
        self.inference_dialect().is_some()
    }

    /// The output cap a cache-keepalive ping sends (docs/transport.md). Anthropic
    /// honours `max_tokens: 0`, which reads the prefix and generates nothing. The
    /// Responses dialect has no zero (the Codex backend refuses `max_output_tokens`
    /// below 16). OpenRouter treats 0 as unset and lets the model generate to
    /// `end_turn`, but honours a positive cap on most upstreams, and OpenAI models
    /// behind it floor the cap at 16, so 16 is the smallest cap that costs the same
    /// everywhere. An upstream that ignores every cap is left to the keepalive's
    /// affordability check (the measured models are in docs/transport.md).
    pub fn keepalive_output_cap(&self) -> u32 {
        match self {
            Self::OpenRouter
            | Self::OpenAI
            | Self::OpenAICodex
            | Self::Custom(WireDialect::Responses | WireDialect::Completions) => 16,
            Self::Anthropic | Self::TypeSafe | Self::Custom(_) => 0,
        }
    }

    /// The Responses request shape, for providers on that dialect.
    /// How long this provider's prompt cache keeps an entry after its last write or
    /// read, as far as anything says (docs/transport.md): the keepalive's cadence
    /// derives from it, and a ping that lands after expiry pays the full uncached read
    /// it exists to avoid, while one that lands early costs a cached read. OpenAI
    /// documents 30 minutes since the last write or reuse for GPT-5.6+, but a
    /// gpt-5.6-luna ping 16 minutes after its last request read cold, and the corpus
    /// has the 5.6 family mixed past 7 minutes, so its two arms take 10. Anthropic's
    /// default is 5 minutes. Everything else documents nothing, and OpenRouter's
    /// endpoint metadata carries no lifetime, so 5 minutes stands as the conservative
    /// reading.
    pub fn cache_ttl(&self) -> std::time::Duration {
        match self {
            Self::OpenAI | Self::OpenAICodex => std::time::Duration::from_secs(10 * 60),
            Self::Anthropic | Self::OpenRouter | Self::TypeSafe | Self::Custom(_) => {
                std::time::Duration::from_secs(5 * 60)
            }
        }
    }

    pub fn responses_profile(&self) -> ResponsesProfile {
        match self {
            Self::OpenAICodex => ResponsesProfile::Codex,
            Self::Anthropic
            | Self::OpenRouter
            | Self::OpenAI
            | Self::TypeSafe
            | Self::Custom(_) => ResponsesProfile::Platform,
        }
    }

    /// The spotter model a bare `serve` uses on this provider
    /// (docs/permissions.md): the cheap band of whatever catalog the provider
    /// serves, since the spotter shares the endpoint and a Haiku id is meaningless
    /// on OpenAI. An explicit `--spotter-model` replaces it and still passes the
    /// provider's capability check.
    /// `None` for a custom endpoint: the cheap band of an arbitrary catalog is
    /// not knowable, and guessing a slug would fail at the metadata lookup with a
    /// message about a model nobody chose. Resolution asks for `[defaults].spotter`
    /// instead (`inference/options.rs`).
    ///
    /// Compiled in, so it ages with the build; where the host holds the
    /// provider's listing, resolution asks [`Self::newest_spotter_model`] first.
    pub fn default_spotter_model(&self) -> Option<&'static str> {
        match self {
            Self::Anthropic => Some("claude-haiku-4-5"),
            // OpenRouter ids carry the author: the bare id fails its metadata
            // route before the first request.
            Self::OpenRouter => Some("anthropic/claude-haiku-4-5"),
            Self::OpenAI | Self::OpenAICodex => Some(OPENAI_DEFAULT_SPOTTER_MODEL),
            // The one model family this provider serves. The floating alias
            // rather than a dated slug: TypeSafe publishes `jev-latest` as the
            // entry point and the API takes it directly.
            Self::TypeSafe => Some("jev-latest"),
            Self::Custom(_) => None,
        }
    }

    /// The default spotter read from this provider's cached listing, where the
    /// listing says which models form the cheap band: Anthropic files each model
    /// under a `line`, so the answer is the newest `active` model on the `haiku`
    /// line, by `created_at` (docs/model-catalog.md). `None` where the listing
    /// names no lines, or holds no such model; the caller then takes
    /// [`Self::default_spotter_model`].
    pub fn newest_spotter_model(&self, rows: &[ListedModel]) -> Option<String> {
        match self {
            Self::Anthropic => rows
                .iter()
                .filter(|row| {
                    row.line.as_deref() == Some("haiku")
                        && row
                            .lifecycle
                            .as_deref()
                            .is_none_or(|stage| stage == "active")
                })
                .max_by_key(|row| row.created)
                .map(|row| row.model.clone()),
            Self::OpenRouter
            | Self::OpenAI
            | Self::OpenAICodex
            | Self::TypeSafe
            | Self::Custom(_) => None,
        }
    }

    /// The decision model this provider serves, for the spotter default
    /// (docs/permissions.md): any connection that can reach jev supplies it, ahead
    /// of the main provider's cheap band. Floating aliases on both — the
    /// first-party `jev-latest`, and OpenRouter's `~`-prefixed alias for it — so
    /// a new jev version needs no new build.
    pub fn default_decision_model(&self) -> Option<&'static str> {
        match self {
            Self::TypeSafe => Some("jev-latest"),
            Self::OpenRouter => Some("~typesafe/jev-latest"),
            Self::Anthropic | Self::OpenAI | Self::OpenAICodex | Self::Custom(_) => None,
        }
    }

    /// Every name [`Self::from_name`] accepts, with the hover text the published user
    /// schema shows for it (docs/config-schemas.md): one table for the parser, its error,
    /// and the schema's `enum`, so the three cannot disagree.
    const NAMED: &[(&str, Self, &str)] = &[
        ("anthropic", Self::Anthropic, "Anthropic's API."),
        ("openrouter", Self::OpenRouter, "OpenRouter."),
        ("openai", Self::OpenAI, "OpenAI's platform API."),
        (
            "openai-codex",
            Self::OpenAICodex,
            "The Codex backend, billed to a ChatGPT subscription.",
        ),
        (
            "codex",
            Self::OpenAICodex,
            "Another name for `openai-codex`.",
        ),
        (
            "typesafe",
            Self::TypeSafe,
            "TypeSafe's decision models, which can serve as the spotter only.",
        ),
    ];

    /// Parse a configured provider name. The vocabulary is closed because a provider
    /// selects the endpoint, auth shape, metadata lookup, and wire dialect together.
    pub fn from_name(value: &str) -> Result<Self, Error> {
        let value = value.trim();
        Self::NAMED
            .iter()
            .find(|(name, ..)| *name == value)
            .map(|(_, provider, _)| *provider)
            .ok_or_else(|| {
                let valid = Self::NAMED
                    .iter()
                    .map(|(name, ..)| format!("`{name}`"))
                    .collect::<Vec<_>>()
                    .join(", ");
                format!("provider {value:?} not recognized: valid values are {valid}").into()
            })
    }

    /// The published user schema for a connection's `provider` (docs/config-schemas.md).
    pub fn name_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
        crate::config_schemas::documented_enum(
            Self::NAMED.iter().map(|(name, _, doc)| (*name, *doc)),
        )
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
    ///
    /// [`ImagePlacement::AfterInterposedTurn`]: crate::record::project::ImagePlacement::AfterInterposedTurn
    // The equal bodies are coincidences: `TypeSafe` and `Custom(SystemOne)` take
    // `InToolResult` because no turn is ever projected for them, not because their
    // dialects place images there, and the OpenRouter default agrees with the portable
    // Messages shape without sharing its reason.
    #[allow(clippy::match_same_arms)]
    pub fn image_placement(&self, model: &str) -> crate::record::project::ImagePlacement {
        match self {
            Self::Anthropic | Self::OpenAI | Self::OpenAICodex => {
                crate::record::project::ImagePlacement::InToolResult
            }
            Self::OpenRouter if is_gemini(model) => {
                crate::record::project::ImagePlacement::AfterInterposedTurn
            }
            Self::OpenRouter => crate::record::project::ImagePlacement::FollowingUserBlock,
            // The portable shape on Messages: an unknown endpoint may be a bridge
            // to an upstream that drops `tool_result` images, and a following user
            // block survives that translation. Responses has one shape.
            Self::Custom(WireDialect::Messages) => {
                crate::record::project::ImagePlacement::FollowingUserBlock
            }
            Self::Custom(WireDialect::Responses) => {
                crate::record::project::ImagePlacement::InToolResult
            }
            // Completions places a result's images itself, in the `user` message
            // after the run of `tool` messages (`api/completions.rs`).
            Self::Custom(WireDialect::Completions) => {
                crate::record::project::ImagePlacement::FollowingUserBlock
            }
            // Projection placement is read while encoding a turn, and a provider
            // with no inference dialect never has one encoded for it. The arm
            // exists because the match is exhaustive, not because the value means
            // anything here.
            Self::TypeSafe | Self::Custom(WireDialect::SystemOne) => {
                crate::record::project::ImagePlacement::InToolResult
            }
        }
    }

    /// Which block this provider's cache breakpoint attaches to
    /// (docs/log-and-projection.md). Anthropic accepts `cache_control` on any
    /// block, so the marker sits at the true wire tail; OpenRouter translates the
    /// marker for the upstream serving the slug, and a non-text carrier is the
    /// shape that translation does not survive on OpenAI-ish endpoints.
    ///
    /// `anthropic/` slugs on OpenRouter take the native `TrailingBlock`: the
    /// marker passes through to Anthropic, whose cache reads only up to a
    /// marker, so a text-block marker froze the read for a whole tool loop
    /// while a trailing one advanced it every round (docs/log-and-projection.md).
    ///
    /// Gemini slugs on OpenRouter take no marker at all
    /// ([`BreakpointPlacement::None`](crate::record::project::BreakpointPlacement::None)):
    /// a marker there becomes a Gemini explicit cache that a tool loop cannot
    /// advance, while Gemini's implicit cache tracks the growing prefix unmarked
    /// (docs/transport.md).
    ///
    /// Fixed at boot from the provider and slug, exactly like
    /// [`Self::image_placement`], so projection-plus-placement is deterministic
    /// within a session.
    // `TrailingBlock` is the answer for three unrelated reasons: Anthropic's native
    // placement, a custom Responses endpoint's own marker, and the arm that exists only
    // because nothing projects a turn for the two providers that serve none. The named
    // Responses providers reach it with their placement unused, since they place their
    // own marker.
    #[allow(clippy::match_same_arms)]
    pub fn breakpoint_placement(&self, model: &str) -> crate::record::project::BreakpointPlacement {
        match self {
            // Responses places its own marker (`api/responses.rs`); the Messages
            // placement is unused there.
            Self::Anthropic | Self::OpenAI | Self::OpenAICodex => {
                crate::record::project::BreakpointPlacement::TrailingBlock
            }
            Self::OpenRouter if is_gemini(model) => {
                crate::record::project::BreakpointPlacement::None
            }
            Self::OpenRouter if is_anthropic_slug(model) => {
                crate::record::project::BreakpointPlacement::TrailingBlock
            }
            Self::OpenRouter => crate::record::project::BreakpointPlacement::LastTextBlock,
            // A text carrier is the marker placement that survives translation to
            // an OpenAI-ish upstream, which an unknown Messages endpoint may be.
            Self::Custom(WireDialect::Messages) => {
                crate::record::project::BreakpointPlacement::LastTextBlock
            }
            Self::Custom(WireDialect::Responses) => {
                crate::record::project::BreakpointPlacement::TrailingBlock
            }
            // Completions has no per-part cache marker; servers cache the prefix
            // implicitly.
            Self::Custom(WireDialect::Completions) => {
                crate::record::project::BreakpointPlacement::None
            }
            // Unreachable for the same reason as `image_placement`'s arm: nothing
            // projects a turn for a provider that serves none.
            Self::TypeSafe | Self::Custom(WireDialect::SystemOne) => {
                crate::record::project::BreakpointPlacement::TrailingBlock
            }
        }
    }

    /// The lowercase name, as configuration spells it — for error messages and
    /// boot output.
    pub fn name(&self) -> &'static str {
        match self {
            Self::Anthropic => "anthropic",
            Self::OpenRouter => "openrouter",
            Self::OpenAI => "openai",
            Self::OpenAICodex => "openai-codex",
            Self::TypeSafe => "typesafe",
            // Configuration spells this one as two keys — `provider = "custom"`
            // plus `dialect` — so unlike the others this is a display name rather
            // than a literal config value. The dialect is folded in because the
            // bare word says nothing about what the session will speak, and this
            // is what `GET /state` and the boot line report.
            Self::Custom(WireDialect::Messages) => "custom-messages",
            Self::Custom(WireDialect::Responses) => "custom-responses",
            Self::Custom(WireDialect::Completions) => "custom-completions",
            Self::Custom(WireDialect::SystemOne) => "custom-systemone",
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
            // Anthropic's scale is already 0..1, and the 0..1 range is the subset
            // both scales accept, so a custom endpoint on either dialect cannot be
            // sent an out-of-range value. An endpoint that wanted the wider range
            // reads the whole scale as half-strength, which is a dull knob rather
            // than a 400.
            Self::Anthropic | Self::Custom(_) => v,
            Self::OpenRouter | Self::OpenAI | Self::OpenAICodex | Self::TypeSafe => v * 2.0,
        }
    }
}

/// An Anthropic model routed by slug (`anthropic/claude-…`), whose marker
/// OpenRouter passes through unchanged (docs/log-and-projection.md).
fn is_anthropic_slug(model: &str) -> bool {
    model.trim().to_ascii_lowercase().starts_with("anthropic/")
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
